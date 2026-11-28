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
- Amended: decision 8 (28 November 2026) adds the permanent input refusal of issue 441, and decision 9 (2026-11-28)
  adds a bounded, verified streaming read of a retry's whole text for its first production consumer (T12). Decisions 1
  to 7 stand as written; decision 9 adds a port trait and a supertrait edge and changes no signature of decisions 1 to 4.
- Scope of the decision: contracts, and the memory and file references that meet them. The change that carries this
  ADR implements decisions 1 to 4 in `orch-core`, the memory fake and the file reference and registers the shared
  tests on both (decision 6 names them). Nothing here is wired into the running daemon. When it was written no SQLite
  adapter carried these contracts; the spec records that the shared retry scenarios have run on SQLite since #385
  (`sqlite_retries_v2`), and the daemon still uses the file backend.

## Context

Facts read from `main` at `07b5f967`, the baseline before this change, after the retry enumeration (#191) and the
reference conformance reconciliation (#196) landed and #130 closed:

- **Pending messages.** `Filter::Messages { recipient }` selects by recipient only. 1.0's
  `MessageStore::list_pending` (`crates/orch-infra/src/storage/message.rs`) returns a message only when its status
  is `pending`, its `to_agent_id` is exactly the agent (a broadcast with no recipient is never pending for anyone)
  and it is not expired. `is_expired` is `expires_at < clock.now_iso()` on the two strings: strictly less, so a
  message whose expiry equals the clock is still pending, and an expiry that is not a date is compared as text,
  never parsed. A consumer that filtered a page client-side could miss every match on a page of rejected rows.
- **Per-task run order.** The port's `Sort` has no run order. The runs list defaults to `started_at` descending then
  id (PR #225 later made the default of a task-filtered list the numeric attempt order; see decision 2). The spec's table promised "per task: `run_no`", but `Run` has no `run_no`: it carries `attempt: u32`, and the
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
   family's other sorts are as for `Filter::Messages` (`Default` and `Id`).
2. **Per-task run order has an explicit sort, `Sort::Attempt`.** Add `Sort::Attempt`. It is offered only with
   `Filter::Runs { task: Some(_), .. }` (an `agent` filter may accompany it) and orders by `Run.attempt`
   numerically, descending, then the run id's bytes ascending. It is refused as `InvalidPage` before any paging
   work for `Runs` with no task, for every other family and for every other filter. The default of a run list whose
   filter names a task is the same key, but it is not decided here: PR #225 and the spec's "Run and goal default
   order" (the lead's decision of 2026-11-23) settled it, so `Attempt` is the explicit form of the order that list
   already has by default, and the two are different sorts for a cursor. `Sort::Default` for a run list with no task
   stays `started_at` descending, then id, so no caller of an untasked list changes. The spec's `run_no` in the sorts
   table is corrected to this: the port's number is `Run.attempt`, not a schema column.
3. **A retry owns its whole `error` text.** This is the lead's decision of 2026-11-23, option A of the two put to
   it, taken under goal decision 7, R97 and ADR 0017 decision 11. `Retry` keeps its shape, and
   `Retry.error` is the complete logical text, including for a retry that has no surviving run. The text is
   measured in UTF-8 bytes. Up to and including 65,536 bytes it may be stored inline. Above that, the writer
   publishes the complete bytes to the blob store first and commits the hash in the retry row, and a read resolves
   the hash and returns the original text byte for byte. A failed publication, or a missing or corrupt blob at
   write time, fails the write and leaves the previous row exactly as it was. A reference that cannot be resolved
   at read time is an error on `read_retry`, `read_due_retries`, `read_retries`, `get_retry`, `due_retries` and
   `list_retries`, never an empty, shortened or partial row. A call fails when a row it would return cannot be
   resolved; a row the call does not select (a future deadline in a due read, or a row past the limit) is not
   resolved, so a damaged row outside the selection never fails a read that does not return it. The three async
   snapshot reads of the file reference and the disk-backed fake do that resolution (blob reads of up to 64 MiB an
   object) on the blocking boundary, on a clone of the snapshot's retry state and never on the executor; a cancelled
   join is a typed storage error. Nothing truncates, and `Retry::validate` no longer refuses long text (it keeps refusing an empty task,
   attempt 0 and a deadline before the epoch). The
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
   sweep may remove it). The physical encoding of the mapping is the adapters' to choose; this decision fixes what it
   must hold and when it is checked. The reference adapters' choice is the next paragraph, and T10 may choose another
   for SQLite so long as it holds the same facts.

   **The reference mapping.** The mapping is `{sha256, chunks}`: `sha256` is the SHA-256 of the complete UTF-8 value and
   `chunks` is the ordered list of the blob hashes that hold it, one entry per position, each the 64 lower-case hex
   digits that `BlobHash` prints. The file reference cuts every 67,108,864 bytes from the start, so every chunk but the
   last is exactly that size and the last is the remainder. The mapping adds no public type or constant to the port:
   `Retry` and the retry traits keep the shape `docs/TRAITS-FROZEN.md` lists, and the type that holds the mapping is
   private to each adapter.
   - *File reference.* A retry row's `error` in the project's document is one of three shapes, chosen by length: a
     JSON string (inline, at most 65,536 bytes); `{"sha256": H}` (one object, named `H`, which is also its content
     hash); or `{"sha256": H, "chunks": [C1, C2, ...]}` (more than 67,108,864 bytes, `H` the whole-value hash and
     each `Ci` an object in the project's blob store). A stored mapping with any other field, single or chunked, fails
     the load of the document as a parse error and leaves it unchanged.
   - *Memory fake.* `FakeStorage::default()` is the **logical** fake: it keeps the whole string in memory, never
     publishes, and makes no claim about backing, publication order or damaged references. A test that needs those
     builds the **disk-backed** fake with `FakeStorage::with_retry_blob_store(store, hash, chunk_bytes)` (feature
     `test-support`), which publishes through the injected `BlobStore`, cuts at the `chunk_bytes` it is given and keeps
     `{hash, chunks}` privately; the conformance fixture `blob_memory_backend` injects the disk adapter with
     `MAX_BLOB_BYTES` as the chunk size. In that form every text over 65,536 bytes has a chunk list, which is one entry
     when it fits one object.
   - *Order of events.* In the file reference and the disk-backed fake, `put_retry` publishes every object and reads
     each back and compares it before it stages the row, so a failed publication or a damaged object fails the call
     and leaves the staged row as it was. The row commits with the unit of work. A unit of work that rolls back, or a
     row that a later put replaces, leaves its objects unreferenced for the blob store's sweep.
5. **The frozen record follows the landed port.** `docs/TRAITS-FROZEN.md` records every public v2 trait, trait
   method, type, constant, free function and inherent method of `orch_core::store` that is on `main` (the
   `test-support` fakes are not listed), including the `ReadSnapshot` and `UnitOfWork` supertraits and the expected
   revision that `update`, `delete` and `move_to_end` take, and the three additions above once they land. The 1.0
   traits stay active and unchanged until T25 removes the product file adapter.
6. **Verification names follow the tests.** The spec's rows for R7, R117 and the caller part of R132 name the
   bare filters the tests have, with the registrations each must select, and the plan lists the 12 selections.
   The shared scenarios of this change are registered once and run on each reference:
   - the five consumer scenarios `pending_messages_filter_status_recipient_and_lexical_expiry`,
     `pending_messages_page_matches_before_count_cursor_and_bound`,
     `run_attempt_order_is_numeric_task_scoped_and_keeps_default`,
     `run_attempt_pages_keep_snapshot_and_bind_every_query_field` and
     `run_attempt_sort_refuses_every_unscoped_or_other_family_filter`, in `v2_consumers.rs`, registered as
     `memory_v2`, `queued_memory_v2` and `file_v2` (3 registrations each);
   - the three retry scenarios `retry_whole_text_round_trips_at_the_utf8_boundary_without_a_run`,
     `retry_whole_text_replacement_rolls_back_and_snapshots_keep_original` (which also replaces a long text by a short
     one and a short one by a long one) and `retry_chunked_text_survives_restore_without_a_run` (a 67,108,865-byte
     text and a fixture restore to a new lineage), added to the retry suite, which now has 21 scenarios registered as
     `memory_retries_v2`, `blob_memory_retries_v2` (the disk-backed fake) and `file_retries_v2`;
   - the physical, fault and boundary checks in `storage::v2::tests` of `orch-infra`: the file reference's backing
     (`retry_text_boundary_is_backed_before_the_hash_row_and_reopens_whole`), damaged and missing blobs
     (`retry_missing_and_corrupt_blobs_fail_all_read_paths_on_both_references`), a refused or damaged publication
     (`retry_publication_refusal_preserves_prior_rows_even_when_caught_on_both_references`), the 64 MiB object
     boundary, a damaged or missing tail chunk on all six retry reads and a reordered file mapping on all six
     (`retry_object_limit_is_not_a_diagnostic_cap_and_chunk_order_is_verified`), a later chunk failing
     (`retry_later_chunk_failure_keeps_prior_row_and_earlier_published_bytes`), repeated chunks
     (`retry_repeated_chunks_reconstruct_every_occurrence_on_both_references`), published bytes that survive a
     rollback with no row naming them (`retry_rollback_keeps_published_bytes_without_a_reference_on_both_backed_references`),
     a hand-made one-object row read after a reopen
     (`retry_existing_single_object_encoding_and_wrapper_extensions_remain_readable`), selection before resolution
     (`retry_selection_does_not_resolve_excluded_damaged_rows_on_both_references`: a healthy row and a damaged row past
     the deadline or the limit; due reads at a fixed `now` and at limit 1 succeed on every snapshot and unit-of-work
     read, the damaged row's point read and the page that reaches it fail, and, on a current-thread runtime, a guard
     on the blob store fails a snapshot read that touches a blob on the executor thread) and a hand-edited single or
     chunked mapping with an extra field
     (`retry_stored_text_unknown_fields_are_refused_for_single_and_chunked_objects`). The backing, one-object and
     unknown-field tests run the file reference only; the other seven each run the file reference and the disk-backed fake;
   - one private unit test in `orch-core`, `store::retries::fake::tests::reordered_retry_chunks_fail_every_memory_retry_read`
     (feature `test-support`). The disk-backed fake's chunk mapping is private, so a reordered mapping cannot be made
     through its port; the test swaps two chunks of that mapping and requires `get`, `due` and `page` of its private
     retry state, which all six port reads call, to fail the whole-value hash. Its blob store is a reversible
     text-name double, so it proves that guard and not SHA-256 or a damaged object, which the tests above do with
     the real `DiskBlobStore`.

   This ADR records the names and counts, not results: a run is a receipt of the PR that carries the change, and the
   head it belongs to is that PR's manifest. R8 to R15 and R97 keep the inventory that the #130 crosswalk
   reconciled.
7. **Who builds what.** This change implements decisions 1 to 4 in `orch-core`, the memory fake and the file
   reference, with the shared tests registered on both. T10 owns the SQLite representation of all three
   (the retry blob and chunk mapping, the `PendingMessages` statement, the attempt index) and the adapter registration of
   the shared suites. T12 owns the finish and enqueue caller and lease-bound finish, T14 the dispatch order, T21
   the legacy import (a whole 1.0 error, rebuilt into chunks when it needs them), and T23 the backup, restore and
   export round trip of every reachable chunk. Each consumer keeps the obligations listed in the plan.
8. **A caller value that storage cannot represent is a permanent, payload-free refusal (issue 441).** This is the
   lead's T12 first-use decision of 28 November 2026, and the lead's disposition of the same day (option A) for what
   issue 441 covers now. T12's first Task write and retry classification need to tell a caller input that can never be
   stored from a fault that may clear, and on SQLite the two were both `Backend` with a fixed message.
   - *The variant.* Add `StoreError::Unrepresentable`, a unit variant. Its text is constant, it has no source, and it
     carries no input, field name, bound or backend text. It says that retrying the same value cannot succeed.
   - *The contract it enforces now (ordinary entity input).* The memory fake, the file reference
     and SQLite refuse the same Task values and the same Context provenance and accept the same boundary values. A `Task`'s `id`, `assignee`, `goal_id`, `created_at` and `updated_at` are at
     most 65,536 UTF-8 bytes, and its two times are the canonical `YYYY-MM-DDTHH:MM:SS.sssZ` from
     `0000-01-01T00:00:00.000Z` to `9999-12-30T22:00:00.999Z`, inclusive (the spec's "Task format v1 (SQLite)",
     "Time", and "Staged SQLite goal timestamp and position limits"). A Context `create` refuses a caller whose identity,
     or whose Task run, is over 65,536 UTF-8 bytes, because both are stored as provenance; an `update` preserves the
     stored provenance and never reads the caller's. The references validate in `create` and `update`; SQLite already
     refused these values natively.
   - *Nothing changes when it refuses.* The row, revision and position are as before, no position is consumed, and a
     caught refusal leaves the rest of the unit of work intact; a returned one rolls the unit back.
   - *Mapping on SQLite.* Both public converters map the native `SqliteError::Unrepresentable` to the variant after
     the failure state has observed it, and drop its text. It is an ordinary class that latches nothing. Every other
     arm keeps its classification.
   - *Faults and damaged data stay apart.* A transient backend fault stays `Backend` and a retry after it clears
     succeeds; persisted data that fails to decode stays a decode error on the file reference and the fatal
     logical-corruption cause on SQLite. None of them is an input refusal.
   - *What the consumer does.* It matches the variant exhaustively, does not reschedule that input, and never infers
     permanence from the text of an error or from `Backend`. This decision adds no consumer and releases none.
   - *What is held.* The identity of a claimed Run is not covered. SQLite's native guard binds `task_id` and `agent_id`
     of a Run update to a persisted claim tuple and answers the same native refusal, which the converters now map; the
     two references have no such guard, ordinary entity creation produces no claim tuple, the port exposes no durable
     claim allocation, and the references remove a released lease, so the presence of a lease cannot tell them that a
     Run was claimed. The guard stays as it is and no reference metadata is added. T12's claim-allocation lifecycle
     task owns the case and must show, before the first path that relies on a claimed Run's identity is enabled, the
     same typed refusal and the same valid same-identity update on all three adapters (spec and plan). Issue 441 stays
     open for it, and the change that carries this decision refers to it and does not close it.
   - *Release.* The first T12 Task write or retry classification that depends on this variant is released when the
     controller records a reviewed, accepted producer commit that is part of the consumer's base, and the public-port
     Linux tests across the three adapters for the same valid boundary and refused inputs, content-free errors,
     rollback and a separately classified backend fault have been run on it. The plan's "Permanent input refusal
     (issue 441)" lists them.
9. **The private retry read is a bounded, verified stream.** This is the lead's T12 first-use decision of
   2026-11-28. It keeps decision 3's promise (the whole text of a valid stored retry is recoverable, also after its
   run is gone) for the first production consumer, a diagnostic read, without a whole-text allocation and without a
   new logical-size ceiling. It adds one trait to `orch_core::store::retries::stream` and makes it a supertrait of
   `ReadSnapshot`; it changes no method signature. `read_retry`, `read_due_retries`, `read_retries`, `get_retry`,
   `due_retries` and `list_retries` keep their contract and their whole-value results, and a caller that accepts a whole
   `String` keeps using them. The memory fake (logical and disk-backed forms), the file reference and SQLite implement
   it; no daemon path calls it.
   - *The trait.* `RetryTextRead::read_retry_text(after, limit, sink, cancellation)` selects exactly the rows, the
     order and the next cursor that `RetryRead::read_retries(after, limit)` selects from the same snapshot, and
     delivers each row's text to a caller-supplied `RetryTextSink` in windows of at most 65,536 bytes
     (`RETRY_STREAM_WINDOW_BYTES`) instead of returning it. A window is a byte slice, not a `str`: a UTF-8 character
     may span two windows and two objects. Every byte and every chunk occurrence is delivered, in order, so identical
     chunks are delivered once per position. The sink's callbacks are synchronous, so a slow sink applies
     backpressure across rows. A row that the call does not select is not read, damaged or not, as in decision 3.
   - *Before any payload is read.* Every selected row is checked before the first byte reaches the sink, and the
     lengths are summed for the page with checked arithmetic (an overflow is `Resource`). Stored metadata that is not
     valid is damage (`Corrupt`). SQLite compares each chunk's declared size with the registered blob of the same
     snapshot and charges the encoded document before it decodes it (see admission). The disk-backed memory fake checks
     the chunk sizes it recorded when it wrote the row. The file reference stores no object length in a text (a
     single object records its hash, and a split text records the whole-value hash and the ordered chunk hashes), so
     it reads one from the object: after it has validated the syntax and structure of every selected reference, it
     opens each selected object through the confined directory, which refuses a symbolic link and a file that is not
     regular, and takes the length from the open descriptor (`BlobStore::stream_len`, metadata only). It reads no
     payload, hashing included, until the whole selected page has passed, so a page whose last reference fails reads
     nothing. Every adapter counts every occurrence of a repeated chunk and keeps the lengths for the verification,
     charged to the counter. This adds no stored field and no format.
   - *Verification.* While streaming, the adapter reopens each object by the same confined route, rechecks its type and
     its length against the length it kept (the declared size on SQLite), reads to the end of the object, and checks the object's hash, the whole
     value's hash and length, and the UTF-8 of the joined bytes incrementally across windows and objects (an invalid
     sequence, or a character cut at the end of the value, is damage). An object replaced between the preflight and the
     read, including by one of the same length, fails on the hash or on the length. Success needs the last of these
     checks. The sink's `begin` and `write` receive **provisional** bytes, including for rows before one that later
     fails. Only a returned `RetryTextPage` (the row count, the summed logical bytes and the next cursor) authorizes the
     consumer to publish its output or to advance its cursor. On any failure, a cancellation or a dropped future the
     consumer discards every provisional byte of the page and the cursor stays where it was. A consumer never collects
     the stream into an unbounded string and never acts on a prefix that was not verified.
   - *Admission, not a ceiling.* A request's logical length is never a reason to refuse. Each adapter owns one
     `RetryStreamAdmission`, a checked shared byte counter that it keeps across snapshots and concurrent reads, queued
     work included, and charges for the working memory that is live (a window, the selected rows' metadata, the bytes a
     sink retains). Its capacity is `RetryStreamAdmission::default()`, 64 MiB (67,108,864 bytes) per adapter instance,
     and no setting changes it. It hands the counter to both sink callbacks, so a sink that must hold bytes charges the
     same counter, and a caller cannot replace it. Allocation is fallible (`try_reserve_exact`). A charge or an
     allocation that fails is `RetryStreamError::Resource`, and no page completes. A permit is released exactly once,
     when the work that holds it exits, also after the requesting future was dropped. SQLite reserves, for each selected
     row, eight times the encoded document, plus the task key and 4,096 bytes, before it decodes the document, so a
     decoder's strings, chunk capacity and field keys are inside the charge before they are allocated. The counter
     covers payload and selection metadata: not allocator bookkeeping, not process memory, not the database, the
     existing snapshots or the control allocations of a call; the `Arc` of `new` is not allocated fallibly. On the
     synthetic page of five 2 MiB retries (10 MiB in all) that the public matrix reads, the highest charge a sink saw
     in its callbacks was 66,710 bytes on the memory fake in both forms, 67,245 on the file reference and 96,157 on
     SQLite. Those are charged bytes sampled at the callbacks, not measurements of resident memory. The allocation proof injects a refusal at the buffer
     allocation boundary of the counter (`fail_next_buffer_allocation`, `test-support` only): it shows that a refused
     buffer is `Resource` and releases its charge, and it does not simulate the process or the global allocator running
     out of memory.
   - *Cancellation.* The adapter derives a child of the caller's `CancellationToken` and holds its drop guard across the
     await, so dropping the future cancels its worker and cancelling one read never cancels another. The worker
     checks the token before each read, before each sink callback and before it returns success. The snapshot outlives
     the blocking work, and the worker's admission stays charged until the worker has actually exited. A test fills
     the adapter's counter from inside a blocked sink callback, then drops the future (or fires the token), and shows
     that the counter stays full, that a second read on the same adapter is refused with `Resource`, and that the
     counter returns to zero only after the callback has returned.
   - *Errors.* `RetryStreamError` has four content-free variants: `Resource` (admission or allocation, never damage),
     `Cancelled`, `Corrupt` (persisted data that fails verification) and `Store`
     (any ordinary `StoreError`, so the classifications of decision 3 and of #441 survive). A selected object that is
     missing, is not a regular file, has the wrong length or the wrong hash is `Corrupt`; an input or output fault on
     an object that is present is `Store(Backend)` with a constant text. A sink's error is latched: an implementation
     never returns success after a callback failed, and stops calling the sink once it sees cancellation.
   - *The blob port.* `BlobStore` gains three provided methods (`stream_len`, `read_windowed`, `with_stream_hasher`) and
     the trait `BlobStreamHasher`, so a store written against the earlier port compiles unchanged. A store that does not
     implement them refuses the stream with `Store(Backend)` and a constant text, and the stream never falls back to
     the whole `get`. The hasher is lent to a closure on the stack, so no boxed hasher is allocated. `DiskBlobStore`
     implements all three.
   - *What stays as it was.* No stored format, schema or setting changes. The 4 MiB serialized-manifest limit on the
     SQLite write side, and the error a write over it returns, are not changed here: that disposition is the
     controller's, under open issue [#546](https://github.com/yuxhuang/r-orch/issues/546), and the cap is no logical
     length ceiling and no reason to map a write refusal to this read's `Resource`.
   - *Owners and release.* A storage-port builder owns the trait and the three adapters; the T12 owner owns the first
     consumer and its bounded sink. This decision releases no production read. A read is released when the accepted
     trait and adapter commit is part of the consumer's candidate, the consuming call has passed the independent code
     review and the security review that its surface requires (the security reasoning and evidence stay private), the
     private Linux evidence that the plan's "Bounded retry text read" lists is recorded against that commit, with
     its actual limits, and the controller has the proofs of the two deferred fixtures named there ([#511](https://github.com/yuxhuang/r-orch/issues/511),
     [#512](https://github.com/yuxhuang/r-orch/issues/512)). The other #449 items keep their own triggers.

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
- **Reuse `started_at` for a per-task order, or add a `run_no` to `Run`.** The first is the order of every run list with no
  task and answers a different question (newest first, equal times by id). The second changes the domain type and the wire for a
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
- **Leave SQLite's refusal a generic `Backend` error and let the consumer read the text.** The text is fixed and
  names no cause, so the consumer could not tell an input that fails every time from a fault that clears, and it would
  have to match a message to decide. Rejected for decision 8's typed variant.
- **Carry the field, the bound or the value in the variant.** It would put a caller's text into a log or a diagnostic
  (R101) and invite a consumer to branch on it. Rejected for a unit variant with a constant message.
- **Map the refusal on SQLite only.** The memory fake and the file reference would accept a Task that SQLite refuses,
  so a test could pass on a reference with an input that no real backend stores. Rejected: the references validate
  the same values, and the shared public tests run on all three.
- **Widen SQLite to accept what the references accepted.** A second-precision or offset time would sort differently as
  a native integer than as text, which the spec's "Task format v1 (SQLite)", "Time" rules out, and a key over 65,536
  bytes cannot be kept out of its native projection. Rejected: the format keeps its canonical form and bounds.
- **Close the claimed-Run case in this change by adding claim metadata to the references.** The port has no durable
  claim allocation, so the metadata would be invented outside the claim path that T12 owns. Rejected: the case is held
  with T12's lifecycle task and issue 441 stays open for it.
- **Collect the text with the whole-value reads, or cap it for the diagnostic.** The whole-value reads allocate the
  whole text (and a page can hold 1,000 of them), and a cap is decision 3's option B again. Rejected for decision 9's
  stream, which keeps the promise and adds no ceiling.
- **Hand the sink a verified prefix as it arrives.** A hash can be checked only after the last byte, so a prefix is
  never known to be the stored text. Rejected: bytes are provisional until the page returns.
- **Let the caller supply the admission budget.** A caller could size a private budget per call and bypass the
  adapter's concurrent limit. Rejected: the adapter owns one budget across snapshots and reads.
- **Raise or remove the 64 KiB window, or read objects whole.** A whole-object read is the 64 MiB allocation that the
  stream exists to avoid. Rejected for a fixed window.
- **Store an object length in the file reference's text, or hash every object during the preflight.** The first
  changes the stored format; the second reads every payload twice and delivers nothing to a page that fails late.
  Rejected: the length comes from the open descriptor, and no payload is read before the page has passed.
- **Add an end-of-row callback.** The row's declared length arrives with `begin`, and the whole page is provisional,
  so a sink has nothing to learn from an end marker that it cannot also learn from the next `begin` or the returned
  page. Rejected: not needed.
- **Map a refused serialized-manifest write to `Resource`.** `Resource` is this read's statement about its own live
  memory; the write-side 4 MiB cap is a different contract, left to issue #546.

## Consequences

- A consumer asks for an agent's pending messages in one bounded call, with a stable cursor and an exact count, and
  a test can fix `now`. A message whose expiry equals `now` is delivered, and one with an unparsable expiry behaves
  as 1.0 behaved. A caller that needs a different notion of time passes a different string.
- `Filter` and `Sort` each gain one serde variant (`pending_messages`, `attempt`). A backend and a wire consumer
  that match them exhaustively must handle them. The 1.1 protocol is not published yet, so no frozen wire changes.
- A SQLite adapter must keep lexical semantics for both the expiry comparison and the attempt tie-break: the
  statement compares text under the binary collation, and an index that orders `attempt` descending then id
  ascending serves `Sort::Attempt` and the same key as the default of a task-filtered run list. For imported runs whose 1.0 attempt repeats within a task (the spec's
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
  enqueue (the shared round-trip scenario models two enqueue calls with that guard and does not test a production
  caller), and T14 must keep dispatching in candidate order and not due-row order.
- A consumer of decision 9's stream has three things to do that the whole-value reads did not ask of it: hold its output
  as provisional until `read_retry_text` returns, discard it on every error including `Cancelled`, and treat `Resource`
  as a statement about this call's memory and never about the stored text (`Corrupt` is the statement about the text).
  The stream has no end-of-row callback: a row ends where the next `begin` is called or the page returns. Its
  cursor is the returned page's `next`, valid only on success. Which consumer policy follows each error is T12's.
- The stream is a port contract with three implementations and a public three-adapter test matrix (four registrations:
  the memory fake in its logical and disk-backed forms, the file reference and SQLite). The plan's "Bounded retry text
  read" lists the tests and the commands that select them. No daemon path calls it, so the evidence is synthetic and
  says nothing about a real diagnostic; the proofs of [#511](https://github.com/yuxhuang/r-orch/issues/511) (an
  unreadable object as an ordinary and as a privileged user) and [#512](https://github.com/yuxhuang/r-orch/issues/512)
  (snapshot admission after a large write) are deferred for this unused producer only and are owed before a runtime
  integration is accepted.
- Every implementor of `ReadSnapshot` must now implement `RetryTextRead`, and a `BlobStore` that implements only the
  three original methods refuses the stream. The first is a source change for any out-of-tree storage adapter; the
  second needs none.
- The chunk path is reference-port behaviour. This decision supplies no measurement above 64 MiB and no runtime
  evidence for any consumer, and it grants no publication or merge authority. The proof it requires is in the
  plan's "T8 consumer contracts (issue 109)".
- The baseline above is `main` at `07b5f967`: it refuses a retry error over 65,536 bytes and has neither variant.
  This change replaces that with the reference behavior of decisions 1 to 4. A reader of `main` before it merges sees
  the baseline, and the spec and the plan state the baseline wherever they state the contract.
- The reference tests do not simulate a SQLite adapter, a daemon consumer, a 1.0 import, a restore that copies files
  or a sweep of the blob store, so none of those is accepted by them. The chunked-text scenario's restore is the
  fixture's lineage switch, and a rolled-back unit's unreferenced bytes are shown to survive the rollback, not to be
  swept. The logical memory fake proves the port's logical rules only; publication and damaged references are proved
  by the file reference and the disk-backed fake, and a one-object row is a hand-made fixture of the documented form,
  because no earlier release wrote one.
- The request-key references have the dispositions of the follow-ups of PR 171 in the spec's request-keys amendment:
  a stored row's epoch is at most `i64::MAX`, the ceiling of `RequestScope::of`; the collection wrapper of the file
  document ignores an unknown field and a row denies one; the persisted check needs the one-hour TTL, so a change of
  `KEY_TTL_MS` migrates or reconciles earlier rows in the same change; and the tie order of rows with an equal expiry
  is a SQLite proof owned by T10 under issue 127. None of these adds a public type or a signature.
- `StoreError` gains a variant and is not `#[non_exhaustive]`, so a consumer that matches it without a wildcard arm
  must handle `Unrepresentable`. The memory fake and the file reference now refuse a `Task` whose times are not the
  canonical millisecond form, so a test that builds one through them uses canonical times: the shared factory's
  timestamp is `2025-01-01T00:00:00.000Z` for that reason, and the `claim_v2` test helper and one SQLite ordering test
  were adapted with it. The mixed-precision SQLite test keeps its deliberately invalid second-precision string, and
  asserts that the reference port refuses it before it shows the native ordering difference. A Run's opaque timestamps
  are not Task times and were not migrated.
- SQLite's variant is wider than the shared rule. Every native `SqliteError::Unrepresentable` that reaches a public
  converter becomes the public variant: also an entity encoding of another family, a message outside the schema's
  bounds on write, a retry diagnostic whose stored document is over its bound, a cursor shape, and the identity of a
  claimed Run (held above). The memory and file references do not enforce those, and the shared tests prove parity for
  the Task and Context rules only. Whether the retry diagnostic document bound needs a refusal on the references, or a
  disposition of its own that keeps R97's whole-text promise, is issue 546.
- Three follow-ups are filed and are not part of this contract: the bound constants are written twice, in `orch-core`
  and in the SQLite codec, with the shared tests as the only guard against drift (issue 544); which refusal wins on
  `update` at a row on the revision ceiling differs between SQLite and the references (issue 545); and the retry
  diagnostic document bound (issue 546).
- Nothing here activates a consumer. The reference tests are the Linux public matrix `permanent_input` of `orch-infra`
  (20 scenarios on each of the three adapters, three file-only and three SQLite-only fault and reopen tests: 66), the
  private unit tests of `orch-core` and the file reference, and the migrated fixtures. This ADR records the names and
  counts that are expected, and a result is a receipt of the PR that carries the change.
