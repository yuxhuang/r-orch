# 0024. Cursors of the eight families other than Task: inline, retained and by lookup

- Status: accepted. The owner ruled on 2026-11-26 (the lead's eighth and ninth escalations of the storage goal); the ruling
  took effect when the owner merged PR #369 (`20f95a672`), which carried it.
- Date: 2026-11-26
- Spec: [SQLite storage for release 1.1](../specs/2026-11-20-sqlite-storage.md) ("Cursors of the eight families other
  than Task" under "Storage port v2", R10 and the `Cursor` row); plan:
  [SQLite storage for release 1.1](../plans/2026-11-20-sqlite-storage.md) ("Non-Task cursors")
- Amends: the `Cursor` contract of [0017](0017-sqlite-storage.md) decision 3 and the cursor text of
  [0020](0020-storage-consumer-contracts.md), for the families other than Task only. It changes no accepted decision's
  wording and supersedes no ADR. Task keeps its cursor and the issue 277 exception of R10.
- Scope of the decision: the contract and its vectors. As built, three private slices are on `main` (PR #376, the inline
  arm; PR #377, the key-taking entry of the three leaves; and PR #383, the table of retained entries and entries by
  lookup). Nothing is wired into the running daemon. The composition (draft PR 385) wires the three into `Storage` on
  SQLite (`sqlite/storage/page.rs`); no vector is signed off through it (plan step 5).

## Context

- A `Cursor` is at most 512 bytes of text (`docs/TRAITS-FROZEN.md`). The SQLite Task cursor carries the last row's
  integer keys and ID in a hex token of at most 256 bytes and, when the ID does not fit, a reference with a fallback.
  That scheme is written for integer keys followed by one long text, and R10 has a narrow exception for it (issue 277,
  approved by the owner).
- The other eight families order by a text and then the ID (teams, runs, messages, goals by `created_at`), by an
  integer and the ID (agents, a task's runs), by the ID alone (context, config, templates) or, for a goal, by five
  keys. Two long texts share the 230 bytes that a token has left, so an anchor with a long name or time cannot be
  carried whole and would end in a fallback that can be refused. A text over 64 KiB is stored behind blob objects (R97).
- R10 requires a walk to return every row whose sort values did not change exactly once, whatever is inserted or
  deleted meanwhile, with no exception beyond Task's. The memory fake and the file reference satisfy it by keeping a
  bookmark for ever and resuming from the stored key, never from the anchor's row. The product cannot keep every
  bookmark for ever.
- The three SQLite leaves that exist take an anchor *record* and refuse an anchor that no longer matches the filter,
  which the references never do.
- The owner ruled on 2026-11-26. D2 of the eighth escalation approved option P (below) with its new refusals and the 64 KiB
  rule, and an independent challenge was run before any wording. The ninth settled two points the challenge raised:
  an anchor over 65,536 bytes resumes by row lookup and is refused if its row is missing or its tuple changed, and an
  inline token expires by the wall clock with defined bounds and a 5-minute allowance, while a retained one uses
  monotonic age.

## Decision

1. **Three kinds of cursor, by the size of the anchor's ordering tuple** (texts at their UTF-8 byte length, 8 bytes for
   each integer, an absent text 0 bytes). *Inline*: the texts fit the token (230 − 8*k* − *t* bytes for *k* integers and
   *t* texts before the identity; the spec tabulates it by list and sort), and the token carries the whole tuple and an
   expiry. *Retained*: not inline and 65,536 bytes or fewer; the token names an entry in the adapter's memory that holds
   the whole original tuple. *By lookup*: 65,537 bytes or more, whether a component is behind blob objects or every
   one is native; the entry holds each text as its SHA-256 and byte length (40 bytes) and each integer by value.
2. **Resume is from the original tuple, never from a row's current state.** Inline and retained cursors resume after the
   original tuple whatever happened to the anchor. A cursor by lookup finds the anchor by scanning the identities of its
   family in the project, unfiltered, in the resuming view, matching byte length and SHA-256, then compares every
   component of its current tuple with the entry. Only an identical tuple resumes; a missing row or a changed tuple
   (a rename, a new time, a recreation at another position) is `InvalidCursor`. No path shifts the boundary without an
   error. The anchor is not checked against the filter, and its revision is not consulted.
3. **The adapter's table** holds retained entries and entries by lookup in memory, never in the database: at most 1,024
   entries and 8 MiB of charge, both applied; oldest-issued first out; an entry's age is read from a monotonic clock
   and is at most 24 hours. The token carries a handle and the adapter's random nonce and binds the project, the
   generation, the family, the filter and the sort. The daemon takes a new generation at every start (R41), so every
   cursor ends at a restart.
4. **Inline time** follows the owner's rule: refuse when `now > expires` or `expires − now > 24 h + 5 min`; equality
   passes. A backward clock step of more than 5 minutes, and a forward step past an expiry, are refusals the owner
   accepted.
5. **The refusals are a complete list** (the spec's "The refusals"). Task is unchanged.
6. **Walkers restart.** A full walk buffers its pages and, on `InvalidCursor`, restarts from the first page a bounded
   number of times, then ends with a defined error (R49; T17).

## Options considered

- **Extend the Task scheme to the eight families (the owner's option Q).** It needs no daemon state and survives a
  restart, but it is the most new code and a wider exception than issue 277: two long texts must share the token's bytes,
  so more anchors end in the fallback that is refused when the successor is deleted too.
- **Bound every sortable value and ID so that every anchor fits inline (option R).** Exact, with no state, but it changes
  a landed contract and what the importer accepts, and the lengths in real projects are not measured.
- **Resume a large anchor from its current row.** It shifts the boundary without an error when the row's sort values
  changed (a renamed team skips its two unchanged successors). It was withdrawn after the challenge and is the
  failure that decision 2 exists to prevent.
- **Pin a large anchor by its blob hash and resume from the original bytes (option 1 of the ninth escalation).** Exact across
  change, deletion and recreation, but it is not free of refusals and it rests on work that does not exist: a native
  text has no object to pin; an object published by a write that rolls back has no row to date it; the seven-day blob
  grace is a wall-clock time and the retained age is monotonic, so the two cannot be compared; a text over 64 MiB is
  several objects; and a missing or corrupt pinned object needs a classification that R140 does not give. It would need an
  R10 exception or a change to R60. The owner chose the lookup. It stays possible later as a relaxation, because every
  entry by lookup holds the digest and length of each text.
- **A cap per text, not per tuple.** Fewer refusals and entries up to 131,096 bytes, but it departs from the owner's
  "anchors over 65,536 bytes". The whole-tuple measure was adopted and lookup entries hold digests, so no entry exceeds
  65,536 bytes.
- **Keep a native identity's bytes in the lookup entry and find it with the existing lookup.** An entry would be as large
  as the retained entry it replaces, which defeats the threshold. The scan costs one pass over the family's identities
  in a rare arm, and the format already says speed is not a goal for large keys (R97).
- **No expiry on an inline token.** It would add no clock refusal, but the owner ruled that an inline token expires.
- **A durable table of entries.** It would turn a read into a write through the single writer, and a failed writer
  would stop long-anchor paging.
- **Refresh an entry on use, or compare the row's revision.** A refresh still leaves a paused caller exposed and changes
  the rule of oldest-issued first; the revision refuses unrelated edits of the anchor.

## Consequences

- A client that walks a list in a mutable order over very long keys can be refused where 1.0 never was: by expiry,
  eviction, a restart, or a changed anchor. Every refusal is `InvalidCursor` and the rule is one: restart the walk. The
  1.0 shape and order of a CLI list stay, through the walker's restart (T17).
- Up to 8 MiB of the daemon's memory holds anchors. The bounds are the owner's starting bounds, unmeasured against how
  often walks run and against real key lengths. An eviction counter is a follow-up. Before a caller other than the owner
  can list (release 1.3), one caller's walks can evict another's, and a per-principal limit is open.
- The three leaves gain a key-taking entry, `select_after`, that does no filter check on the anchor (PR #377). The
  record-taking `select` and its refusal stay in the leaves until the page PR retires their use. The leaf half of
  P-F1, P-F2, Q8 and N3b is tested there; N3a and every integrated vector remain the page PR's (issue 370).
- The `Cursor` text of the port, R10 and the vectors are in the spec. The page PR names every vector as a test and
  executes the mutants. The wording is on `main` (PR #369). Public paging reaches `main` with the composition (PR 385);
  its named vectors, mutants and the physical corpus (plan step 5) are not claimed.
- The inline capacities are derived from the Task token's constants. The encoder (PR #376) confirms all nine rows of the
  spec's table, and the table needed no correction.
