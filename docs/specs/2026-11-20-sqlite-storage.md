# SQLite storage for release 1.1

status: draft
owner: architect; requirements approval: the owner, at the approval gate that follows the entry steps
date: 2026-11-20
plan: [SQLite storage for release 1.1](../plans/2026-11-20-sqlite-storage.md)
decision: [ADR 0017](../decisions/0017-sqlite-storage.md) (proposed); consumer contracts: [ADR 0020](../decisions/0020-storage-consumer-contracts.md) (proposed)

This spec is longer than the page that [the specs README](README.md) asks for. The goal requires it to fix
names, bounds and the schema split before any code, so those are kept here as reference sections after the
requirements, as the daemon-owned-state spec did for its protocol. The names of operations, commands, settings
and metrics are those of source item O9, which the owner answered as recommended: approved as a set, and
fixed by the merge of the PR that carries this corrected text.

Where this text stands. The first-review text of this spec, of its plan and of ADR 0017 reached `main` on
2026-11-21, when the owner merged the PR that carried it at revision `f5637805`. That text is the draft as
it stood before the reviews: it ends at R100 and AP13, and holds none of the rows added after the security
review and the red team (R101 to R140, AP14 to AP20) and none of the owner's answers. This text replaces
it through a follow-up PR and is pending until the owner merges that PR. The record holds no word of the
owner that the first merge approves this spec, confirms an answer, fixes a name or releases product work,
and this spec reads it as none of those: the status stays `draft`, ADR 0017 stays proposed, and the
conditions of the answers D1 and D8 are unchanged.

This revision also carries the owner's ruling of 2026-11-21 on what a miss of the revised budget selects
(AP13, "Event measurement"), and the corrections that the reviews of 2026-11-21 asked for: R137 to R140,
the verdict, host and platform rules of "Event measurement", and three corrections of fact. The owner
allowed those corrections to be written in one pass. That word accepts no finding, method, number or
result. Five independent reviews read that text at revision `a914e78` on 2026-11-21: one blocked it and
four passed it with conditions. This text answers their conditions: when a path may be selected at all
("What a miss selects"), how the restore's marker is published and its parts are moved (R138), how the
record of a stop is kept (R61, R139), and which new numbers and names are proposals. Five independent
reviews read those answers at revision `3c50293` on 2026-11-21, and each passed them with conditions.

One more revision followed that one, `e8e681e`. It added two answers of the owner of
2026-11-21 and nothing else (record F of "Owner's answers"): "one platform alone" is judged by kind of
limit and population and not by cell ("What a miss selects"), and after a mandatory miss no further data
set is started without the owner's word, whatever the headline, a data set that only fixes a defect
included ("The verdict"). Those two rules are the owner's choice. The text that states them is this spec's. That revision
answered no other condition that a review had left.

The revision after that, `d2fe9b0`, answered the conditions that the five reviews of
`3c50293` left on the restore, on the stop and on one attribution, and it changed no rule that the owner
chose. The restore's last check no longer opens the database, so a stop inside it leaves nothing behind,
and an abandonment that was stopped during its cleanup is finished by `--abandon` (R138). The claim that
no event number is given twice is cut down to the numbers that are committed or recorded (R61). The
ranges of a stop are taken after the writer's last transaction and are cut, when they are replayed, to
the numbers that are not committed (R139). What a sync promises is said apart for a stop of the process
and for a loss of power ("What a sync promises"). And R63 no longer calls the eligibility rule the
owner's ruling. The owner allowed corrections for review findings to be written without a new question
each time (record G of "Owner's answers"). That word closes no finding and accepts no method, number or
result. Five independent reviews read `d2fe9b0` on 2026-11-21, with the changes of `e8e681e`, and each
passed it with conditions.

The revision after `d2fe9b0`, `8954cf6`, changed wording, and no rule, number, limit, method
or step. The loss of event numbers that were given and were neither committed nor recorded is named as an
open item of its own; the text before `8954cf6` counted it with an item of the red team's review that is
about something else (R61). An item of a review is named with its review and the revision that review
read. O1 to O10 with no such name are this spec's own open points ("Open questions"), and the red team's O6
and O8 are neither the O6 nor the O8 that the owner decided. R61 names more of the ways in which a number
is lost. Where the text said that the owner had not read the eligibility rule, it says that the owner has
not ruled on it, which is what the record shows. And a sentence about a review says which revision that
review read. The plan sets these changes against the findings that asked for them. Outside those changes
`8954cf6` is as the five reviews of `d2fe9b0` read it. Two independent reviews, of resilience and of facts,
read `8954cf6` on 2026-11-21 for those changes, and each passed it with conditions.

The revision after `8954cf6`, `ec68890`, wrote in four answers of 2026-11-21 that were given under the
owner's delegation, and a fifth from the same record (record H of "Owner's answers"). The four settle two
things that this spec had marked as proposals and two questions that it had left to the owner. The five
kinds by which the kind reading is applied are adopted, and they
are operative in the match and are not names in a report only. A miss of steps 2 to 4 anywhere in a data set
keeps it from selecting an event path, while an eligible data set in which every miss is a valid miss in a
population that holds events, shown by both platforms in the same cell, population and kind, still selects
the file fallback ("What a miss selects"). And starting the second platform's block after a mandatory miss
on the first, and replacing an `invalid` or `host_not_idle` set of five while such a miss stands, are each
further measurement that needs the owner's word ("The verdict"). The fifth allows the follow-up PR to be
opened as a draft (the row D8 of "Owner's answers"). None of the five is the owner's own words: the owner's
words are the delegation, and record H gives them. That revision changes no number, limit or eligibility
rule, chooses no method, permits no measurement and closes no finding, and every other proposal of AP13
stays a proposal. Selecting a path is not permission to measure it and does not qualify it to ship. Four
independent reviews read `ec68890` on 2026-11-21 (method, red team, facts, resilience): one blocked it and
three passed it with conditions.

The revision that carries this paragraph answers those four reviews, and it changes no rule that the owner
chose or that was answered under the delegation. A data set that has a correctness defect beside an absent
platform or an `invalid` set of five gets its headline and its exit from the one rule of "The verdict", the
defect first, and the text on the two answers about further measurement names no headline of its own.
Record H gives the second answer in the record's own words, and a case shows two kinds of miss, each shown
by both platforms, selecting the fallback. Two things that no answer settles are named as open with what the
owner holds (AP13): what is done with a block that is already running when a mandatory miss is seen, and
whether the blocks of two platforms may overlap. No measurement campaign is started before they are settled.
Which data set a block that the owner allows belongs to is named as open too. This text chooses none of the
three. Step 5
says that a selected fallback is measured only on the owner's word. Record H is corrected in its count, its
time and four phrases. The plan sets each change against the finding that asked for it. Each
review named in this text read the revision named with it and no later one.
Every condition that a review
has left stays open until its reviewer has read the exact text, and so does every number, limit, method
and result that the owner holds.

The revision that carries this paragraph follows the owner's decision of 2026-11-21 on latency (record I of
"Owner's answers"), and it changes nothing else. The corrected text of `34b3b4d` reached `main` on 2026-11-22
(UTC) through the merge of its follow-up PR. This revision changes no status line: what that merge means for
the status of this spec, of the plan and of ADR 0017 is for the approval gate to record (plan task T7). By the
decision, claim latency is not a concern of release 1.1. Run events are stored in SQLite by the batched design
of "Writer lanes" (R63), and the event fallback is not built in 1.1. Deferred, each by a dated note where it
stands and with its text kept as a record: the measurement of deliverable D (R5), both claim-latency budgets,
the method and the selection procedure of "Event measurement" with every proposal in it, and the measurement
of the shipped path as built. Gate line 7 is retired, and the gate of 1.1 is G1 to G6, G8 and G9. The first
run's figures and its FAIL are kept: [issue 98](https://github.com/yuxhuang/r-orch/issues/98) holds them and
links the experiment's files. The owner's ruling and the answers on AP13 (records C, E, F and H) are not
withdrawn. They have nothing to act on until latency is taken up again. No measurement was run for this
revision. It closes no review finding: the findings that exist only for the measurement are parked with it.
Every requirement on the correctness and the safety of events keeps its rule and its tests (R14, R26, R27,
R61, R62, R126, R127, R129, R137, R139, R140). Every test of an event file is due only with the fallback
(R101, R102, R108), the test of its name included. In 1.1 the hostile run ids of "Filesystem names" are
tested on the finish file of R129, the one file that 1.1 names from a run id, through the operations that
write it, recover it and remove it. One independent review read this text at `3a6189a` and passed it on
the condition of that placement, and the security reviewer of those rows agreed to it in writing. Neither
has read the text that makes it.

The revision that carries this paragraph corrects the import contract for `config.yml` and for a damaged
`_index.json`, and changes nothing else. It follows the compatibility evidence on the import's bounded
reader ([issue 146](https://github.com/yuxhuang/r-orch/issues/146)), which found two things. First, this
spec was wrong in fact in three places and ADR 0017 in one: 1.0 reads a `config.yml` whose stream holds no
document as its defaults, and tests that (F18). So the document-count row of "Compatibility with 1.0", the
"three places" of R110, the "three rules" of AP15 and the same words of ADR 0017 decision 21 each missed a
fourth place where the import is stricter than 1.0. Second, the table "Import problem classes" had no row
for an index array with an entry that has no string identity, or with one identity twice, and R123 requires
a row for every class. Four questions were put to the owner on 2026-11-22 (T21-C1, T21-C2, T21-X1 and
T21-X2 of "Open questions"), and a fifth with them (T21-O1). The owner had answered none when this text was
written (record J of "Owner's answers"). Each of the four carried a default for that case, and this text is
drafted from the four defaults. Each default keeps a stop that this spec or the bounded reader already had:
the stricter rule stays, and its refusal now says what 1.0 did with the file. The spec owner's recommendation differs from the
default on three of the four, and the table keeps the two apart. The fifth question carried no default: this
text shows one proposed reading, its alternative and what each does to `run_cap`. None of the five is an
answer, and no row says "decided" for them. The corrections of fact and the two new rows are needed under
every answer. This revision changes no status line and no other rule, no independent review had read it
when it was written, and nothing was built from it.

Corrected on 2026-11-22, after the first independent review of that revision. As first written it had the
refusal of an index array that holds one identity twice say that 1.0 read the index. That is true only of
an array whose every entry is a complete entity of its directory's type; 1.0 rebuilds every other one, an
array of identities alone included (F19). The import judges an entry by its identity alone, so it cannot
know which of the two 1.0 did, and its refusal now states both cases and claims neither. The stop is as it
was, and the import decodes nothing that it did not decode before. The same correction says what the
compatibility tests of issue 146 pin and what they do not (F19), shows the owner how T21-X1 and T21-X2
stand to 1.0 when both are answered A, and mends several smaller statements that the review named. No
rule of the import, no default and no proposal changed with it, and no independent review had read the
correction when it was written.

The revision that carries this paragraph records the owner's ruling of 2026-11-23 on platforms (record K of
"Owner's answers") and changes nothing else. Development, tests and qualification of every release from 1.1
through 2.0 are on Linux only. This text therefore drops, from what is required, each check that needed a
macOS machine: the real suspend and resume on a macOS laptop that R42 once named, the macOS leg of the peer
check, the macOS cold gate, the macOS half of the case-folding tests and the macOS hardware cell of the event
measurement. The rows, notes and figures that say what a macOS run measured, and what the product does on macOS,
are history and stay as they were written. The rows that acceptance rests on are edited: F9, F10, R6, R16,
R31, R42, R88, R89, R91, R118, R136, G1, G2, "Constraints", "Verification environment", "Gate lines" and the
Verification table, the sync note of "What a sync promises", and the notes of AP1, AP12, AP16 and D6. It
changes no requirement statement, no number and no code, and keeps the eight gate lines, the second host for the
peer check (a Linux host), the clock-seam tests and the owner's permission for the live copy.

It does not withdraw the real suspend check on Linux. R42 keeps its second part: a supervised process through a
real suspend and resume of an isolated Linux host that the owner has authorized, tracked in issue 110. That part
is pending: no host has been suspended and no result is recorded. The clock-seam test of R42 injects the clock,
so it proves the order of renewal and expiry and nothing about which operating-system clock the product reads
or what a real suspend does. A physical macOS observation is deferred to the After 2.0 milestone (issue 178).
Record K also puts a duty on the release that ships 1.1 and each later release: say in the README upgrade notes,
the CHANGELOG and the release PR that the release was qualified on Linux only and that macOS is untested until
after 2.0 (R88, R136, and AGENTS.md "Release"). The first independent review of this revision (frozen head
`200da1d4`) found these points, and the revision was corrected and re-synchronised with `main` after it; the
corrected text has not been read by that reviewer.

The revision that carries this paragraph records the consumer contracts of issue 109 and changes no other rule. It
adds three contracts to the port (`Filter::PendingMessages`, `Sort::Attempt` and whole retry text through the blob
store) in "T8 consumer-contract amendment (issue 109)" under "Storage port v2", in
[ADR 0020](../decisions/0020-storage-consumer-contracts.md). It corrects the spelling of the entity operations
(`update`, `delete` and `move_to_end` take the expected revision), replaces `run_no` with `attempt` in the sorts table,
and names the real test filters of R7, R117 and the caller part of R132. `docs/TRAITS-FROZEN.md` now records
every public v2 trait, trait method, type, constant, free function and inherent method of `orch_core::store` on
`main` (the `test-support` fakes are not listed) and, with this revision, the three additions. The same revision
implements the three contracts on the memory fake and the file reference and registers their shared tests (named in
"T8 consumer-contract amendment (issue 109)"); the SQLite adapter and every runtime caller remain with T10, T12, T13,
T14, T21 and T23. The third, whole retry text, includes the chunk mapping for text above one 64 MiB blob object (ADR
0020 decision 4). This spec records the test names and what each shows, not their results: a result is a receipt of
the PR that carries the change. No independent review had read this revision when it was written.

## Source

Verbatim excerpts of the owner's request as recorded in the goal "orch 2.0 phase 1: SQLite storage (release
1.1)". Labels are added for reference and change no wording: S1 and S2 for context, B1 to B15 for the binding
decisions, A to R for the deliverables, G1 to G9 for the gate, X for out of scope, O1 to O10 for the open
points, DL for delivery. Checkout instructions and tracker metadata are not requirements and are not reproduced.

### S1. Where this sits

> orch 2.0 is reached by one release per phase. Each release leaves a single-machine install working from one start command.
>
> - 1.1 storage (this goal): SQLite, a one-way import, `orch db`.
> - 1.2 evidence as stored records, text search (SQLite FTS5), live log follow.
> - 1.3 HTTP/2 over TLS on loopback, with a token.
> - 1.4 authentication: enrolment, client certificates, a project list per identity.
> - 1.5 workers with fenced leases, a managed git hub, daemon-side landing, one daemon serving many projects (opt-in).
> - 2.0 orch-peer is removed.
>
> What 2.0 looks like, so that 1.1 does not paint it into a corner: one central daemon owns all state; workers on any machine dial it, take leases and run agents; every lease carries an epoch that fences a superseded run; a task is done only when the daemon has merged its accepted run ("done means landed"); the agent records a claim and the daemon sets the verdict. None of that is built here. 1.1 lays the storage those phases stand on.
>
> 1.1 is one of two breaking releases: every project must be imported once, one way.

### S2. What 1.0 does today

> - State is files under `.orchestry/`: one YAML per entity, a per-store `_index.json`, `state.json` (running, claimed, a retry queue capped at 100 entries that drops the oldest, stats), `runs/run_*.json` and `runs/run_*.jsonl`, `config.yml`, messages, context, templates, attachments.
> - One daemon per project directory owns that state; every client talks to it over a Unix socket (protocol version 2, 115 typed operations: 48 read, 43 mutate, 11 destructive, 13 control). No client ever starts a daemon; with none answering a client exits 7.
> - Each store offers get, save, list and delete on one entity. Nothing spans entities, nothing checks versions (last writer wins), and lists return everything.
> - Order is behaviour: saving an entity moves it to the end of the index, and agent selection depends on index position. That is why ties between agents rotate.
> - Size on the owner's live project (2026-11-19): about 700 MB of run events for 2,538 runs (about 279 KB per run), and 68 MB of task YAML for 2,979 tasks.

### B. Decisions that bind this goal

> 1. SQLite is the only backend from 1.1. The file backend is removed from the product and kept in the test tree for one release as the conformance reference.
> 2. Import is one-way. The importer never writes to `.orchestry/`, so the way back is that untouched directory with the 1.0 binary.
> 3. The engine reaches state only through a storage port with transactions (units of work), a revision on every entity, and paged lists. Removed from the port: the whole-document state store, the YAML merge-key rule, and "save moves the entity to the end of the index".
> 4. The schema is multi-project (a `project_id` on every table except the daemon-wide ones), but 1.1 still runs one project per daemon.
> 5. One data directory per daemon, mode 0700, with an exclusive lock. A second daemon pointed at it refuses to start and names the holder.
> 6. One task performs every write, one transaction at a time; a pool of read-only connections serves reads from a consistent snapshot.
> 7. Text up to 64 KB may be stored in the database; larger or non-text bytes go behind a blob-store adapter (files on disk now, object storage later).
> 8. `orch db export` writes a file the importer can read back. Every `orch db` command and `orch import` is for the owner, over the local socket only.
> 9. The scheduler sits behind an interface that only decides which task goes to which agent; the engine's claim transaction is what prevents a double dispatch. 1.1 ships one backend: today's in-memory logic, with the same rules and the same rotation.
> 10. Raw run output is pruned after 90 days for successful runs and 365 for failed ones by default; "never" is an explicit setting. Results are never pruned.
> 11. There are no per-project storage quotas. The daemon refuses new work when free disk falls under 5 GiB or 5%, whichever is larger.
> 12. Storage defaults: WAL journal, `synchronous = NORMAL` (FULL is a setting), foreign keys on, `BEGIN IMMEDIATE` for writes, STRICT tables, a busy timeout of about 5 s as a safety net, local disk only.
> 13. Every completed task is recorded as having nothing to land (`landing_state = 'skipped'`) until workers arrive in 1.5. The statuses `proposed`, `landing` and `needs_attention` exist in the schema but nothing enters them before 1.5.
> 14. A project whose agents use the 1.0 `auto` approval policy is imported with a per-project pre-approval policy on, so it keeps running unattended. Only the owner can set that policy. This is implicit trust of the agents and the manual must say so plainly.
> 15. orch-peer is not changed by this goal. It must keep working beside a 1.1 daemon, or the spec must say exactly what breaks and get the owner's approval.

### A to R. Deliverables

> Entry steps, done before any product code:
>
> - A. The spec, the plan and an ADR in the repository, carrying the conditions in this description.
> - B. A storage conformance suite written against the file backend, and the list of every read whose result depends on index order.
> - C. The choice of database library, with the dependency check AGENTS.md requires.
> - D. A measurement of batched event throughput against a stated claim-latency budget.
> - E. A prototype of "claim with a fenced lease", including what happens across sleep and wake.
>
> Then the product:
>
> - F. Storage port v2: units of work, revisions, cursor-paged filtered lists, an explicit ordering column, claim operations, batched event append, a principal on every call.
> - G. The SQLite backend: schema, numbered forward-only migrations, the single writer with priority lanes, the read pool, intent rows for side effects.
> - H. The engine on the new port: each multi-entity operation is one transaction; leases and a retry table replace `state.json`; the maintenance jobs that belong to 1.1.
> - I. Request keys, so a repeated change runs once; revisions and cursors in the API; a protocol version bump.
> - J. The authorization matrix, checked in one place in the operation dispatcher.
> - K. The scheduler interface with today's scheduler behind it, and metrics for tick duration and open tasks.
> - L. The blob store on disk; rendered prompts stored once by hash; attachments as blobs.
> - M. Run events in SQLite, with pruning.
> - N. `orch import`: one-way, bounded, with a dry run and a resume.
> - O. `orch db`: status, verify, query, backup, restore, prune, vacuum, export.
> - P. The attention queue with its first two kinds: disk floor, and dispatch off with work waiting.
> - Q. Usage recorded for every run, whatever its outcome, with the model and the agent CLI version.
> - R. Documentation: manual, RUNBOOK, ARCHITECTURE, README upgrade notes, CHANGELOG.

Changed by the owner on 2026-11-21 (record I of "Owner's answers"): deliverable D is dropped. The goal's
record now reads "D was dropped by the owner on 2026-11-21. The historical experiment is recorded in
[issue 98](https://github.com/yuxhuang/r-orch/issues/98)." The line above is kept as first asked; R5 says what
became of it.

### G. The gate to leave this phase

> Every line needs a named test or a recorded check. The goal is not done while any line is unproven.
>
> 1. The conformance suite is green on SQLite and on the file-backend reference, including list order and agent rotation.
> 2. The importer is verified on a copy of the owner's live project, with a dry run and a resume.
> 3. A backup followed by a restore round-trips the database, the blobs and the repositories as one restorable point. (No bare repository exists in the data directory before 1.5, so in 1.1 that part is an empty directory; the code path must already cover it.)
> 4. An export followed by an import of one project reproduces every row and hash.
> 5. Test principals for a worker and for a task are refused what they must be refused.
> 6. A second daemon on one data directory refuses and names the holder.
> 7. Measured event throughput meets the claim-latency budget, or the file fallback for events ships.
> 8. A dropped and repeated change runs once; the same key with a different body is refused.
> 9. A restore starts a new daemon generation: no epoch is reused, and the restore lists the identities and tokens it re-enables and asks for confirmation.

Changed by the owner on 2026-11-21 (record I of "Owner's answers"): line 7 is retired. The goal's record now
opens the gate with "Each active gate (1–6, 8 and 9) needs a named test or a recorded check." and reads, at 7,
"Retired by the owner on 2026-11-21. The original number is reserved for references to the historical
experiment." The lines above are kept as first asked; "Gate lines" has the gate as it stands.

### X. Out of scope

> Evidence records, quarantine, uploads and text search (1.2). HTTP/2, TLS and tokens (1.3). Enrolment and certificates (1.4). Workers, the worker protocol, the git hub, landing, proposals, many projects on one daemon (1.5). Removing orch-peer (2.0). Postgres, an indexed scheduler, object storage, token budgets, any outbound notification (after 2.0). Windows.

### O. Open points the spec must settle and the owner must approve

> - (O1) How a client finds the daemon and its data directory in 1.1, while there is still one daemon per project directory. The target is that a project is named explicitly (`--project`, `ORCH_PROJECT`, or a pointer file `.orch-project` that holds a project id and nothing else) and never guessed; which release introduces that is not fixed.
> - (O2) Which tables 1.1 creates. The full 2.0 sketch has 50 tables. Columns whose later addition would be a one-way migration must be there from 1.1; whole tables for later phases may arrive by migration.
> - (O3) Whether `orch import` runs with the daemon stopped, and how long the 1.0 files are kept after import.
> - (O4) Whether the daemon starts with dispatch as it was when it stopped (proposed) or always off (1.0).
> - (O5) Whether the maintenance loop is one timer with per-job due times or one timer per job.
> - (O6) How long resolved attention items are kept, and whether a blocking item makes `orch doctor` fail.
> - (O7) Unknown keys in a 1.0 `config.yml`: proposed to be stored in a size-capped legacy object that the daemon never reads, and listed by the dry run. `max_concurrent_agents`: proposed to become an optional per-project run cap.
> - (O8) Request keys are kept for an hour, so their expiry job must ship with them in 1.1.
> - (O9) Names of new operations, commands, settings and metrics. All names in this goal are proposals.
> - (O10) How the fixture corpus for the conformance suite and the importer avoids private data.

### DL. Delivery

> tier=mid, stance=0.5, create PRs. This is far more than one PR: deliver an ordered series of stacked PRs against main, each green and each leaving main fully working. The file backend keeps working on main until the PR that removes it. The owner merges. No tag and no version bump in this goal; cutting 1.1.0 is a separate decision the owner takes when the gate evidence is in.
>
> Approval gate: after the entry steps, present the spec, the measurement, the prototype result, the library choice and the open points above with recommendations, then pause this goal and wait for the owner. Build nothing in the product before that.
>
> A task in this goal that turns out to be larger than one PR of about 3,000 changed lines is split: deliver the first slice, and add follow-up tasks to this goal with their dependencies stated.
>
> Tests never touch a real project's daemon or the developer's real `~/.orchestry`. Anything measured or verified on the owner's live project is done on a copy outside the repository, and no private content from it enters the repository, a PR or a fixture.

Changed by the owner on 2026-11-21 (record I of "Owner's answers"): the measurement is no longer presented at
the approval gate. The goal's record now reads "present the spec, the prototype result, the library choice and
the open points above with recommendations". The paragraph above is kept as first asked.

## Facts this spec rests on

Read from the code at `fb6a1e1` (v1.0.0) unless marked unverified.

| # | Fact | Status |
|---|---|---|
| F1 | The catalogue has 111 operations: 47 read, 42 mutate, 10 destructive, 12 control (`crates/orch-proto/src/ops/mod.rs`, the class table in its tests). S2 says 115 (48, 43, 11, 13); each class is one lower in the code. This spec counts from the code; see added point AP2. | verified |
| F2 | `LOCAL_PROTOCOL` is 2. The wire error codes are `general`, `invalid_arguments`, `not_initialized`, `lock_conflict`, `agent_adapter`, `workspace`, `not_found`, `conflict`, `not_permitted`, `overloaded`, `shutting_down`, `unimplemented`, with exits 1, 2, 3, 4, 5, 6, 1, 1, 2, 7, 7, 1 (`crates/orch-proto/src/error.rs`). | verified |
| F3 | Five stores keep last-write order in `_index.json` (task, agent, goal, context, message). Agent choice breaks ties by that order (`AgentService::find_best_in`); every agent save, including a status change, moves the agent to the end. Team order is directory enumeration and is not a contract. | verified |
| F4 | A run event has a timestamp, a type and data, and no sequence number. Event data is cut at 8,192 characters and a stored line at about 4 KiB (the cut counts characters, so multi-byte text can exceed it). A run records tokens, a pid, its workspace path and the rendered prompt inline; it records neither the model nor the agent CLI version. Nothing prunes runs or events. | verified |
| F5 | Dispatch on or off lives only in memory and is off after every start. `orch doctor` exits 0 when its checks fail and a daemon answers. There is no metrics facility. | verified |
| F6 | The approval policy `auto` (the default) moves a task from `review` to `done` after a successful run without a person. It is not passed to the agent CLI. | verified |
| F7 | `orch-peer` runs inside the daemon on the engine. It reads `tasks/_index.json` and `messages/_index.json` directly and builds a `FileMessageStore` (`crates/orch-peer/src/worker.rs` near line 805, `crates/orch-peer/src/lib.rs` near lines 299 to 316), reads `state.json` through the state store (`worker.rs` near line 868), and keeps its own files `remotes.yml`, `remotes/` and `peer-id` under `.orchestry/`. | verified |
| F8 | The 1.0 daemon takes `flock` on `.orchestry/.daemon-owner.lock` and then writes the PID file `.orchestry/orchestry.lock`. Agent workspaces are under `.orchestry/workspaces/`. | verified |
| F9 | An exclusive `flock` taken through a read-only descriptor conflicts with the 1.0 daemon's lock on macOS and Linux and changes nothing in the file. | unverified; R31's test settles it on Linux. Its macOS half is not required before 2.0 (record K). If it proves false on Linux, R121's detection is the only guard while both binaries can run, and the 1.0.1 alternative of AP16 becomes this spec's recommendation (red-team condition C4) |
| F10 | Suspending the machine does not, by itself, end a local agent process or the daemon's supervision of it. | unverified. Deliverable E models a suspend as elapsed time, and R42's clock-seam test covers the ordering rule on an injected clock; that test proves renewal ordering only, not which operating-system clock the product reads and not what a real suspend does. The check on a real machine is a supervised process through a real suspend and resume of an isolated Linux host that the owner has authorized (D6, issue 110). It is pending: nothing has been recorded, and the fact stays unverified until it is. The isolated macOS machine that this row once named is withdrawn by record K; a physical macOS observation is deferred to the After 2.0 milestone (issue 178) |
| F11 | `orch init` writes `templates/default.md`, `workspace-exclude` and `.gitignore` and creates `logs/` under `.orchestry/` (`crates/orch-daemon/src/bootstrap.rs`). The workspace manager reads `workspace-exclude` for every isolated workspace (`crates/orch-infra/src/workspace/manager.rs`). A search of the product sources found no reader of `templates/default.md` and no writer of `logs/`: the prompt comes from the configuration's template strings or the built-in default. | verified by search |
| F12 | 1.0 reads every YAML document (`tasks/`, `agents/`, `goals/`, `teams/`, `config.yml`) through one guarded reader (`crates/orch-core/src/definition_yaml.rs`, `parse_yaml_bytes`): at most 1,048,576 bytes, at most 64 aliases, at most 200,000 nodes after expansion, counted exactly from the parser's events without expanding anything, and an alias inside its own anchor refused. Only then does `serde_yaml_ng` build the value, and merge keys are resolved (`apply_merge`). It once had a 2 s deadline and removed it, because a deadline cannot stop a parse that is already running. It has no depth rule of its own: `serde_yaml_ng` 0.10.0 accepts 128 nested collections and refuses 129, and `saphyr-parser` 0.2.2 refuses deep flow nesting, each with an error and no stack overflow. A typed field ignores an explicit tag (`id: !custom abc` reads as `abc`, `!!binary YWJj` as the text `YWJj`). Nothing 1.0 writes holds an alias, a merge key or a tag: the types stored as YAML have only unit enums. JSON documents (`runs/*.json`, `messages/`, `context/`, `state.json`, every `_index.json`) are read whole, with no limit on size or node count; `serde_json` 1.0.154 refuses a value nested 128 deep. | verified in the code at `fb6a1e1`; the parser behaviour by a probe outside the repository (the review's 40,001-byte document of depth 20,000 and its 412-byte alias document), macOS |
| F13 | SQLite 3.54.1, the version decision 19 bundles, on a file it did not write: by default a connection trusts the schema, runs views and triggers and is not in defensive mode. With `SQLITE_LIMIT_LENGTH` lowered, reading a stored value over the limit fails (`string or blob too big`) and reading its length does not. A connection opened `mode=ro&immutable=1` reads the one file: it ignores a journal and a write-ahead log planted beside it and creates no file, and it reads a file in write-ahead-log format without its log, which is a stale answer and not an error. An authorizer that denies by default refuses a generated column, a function, a virtual table, `ATTACH`, a pragma and a read of `sqlite_schema` that it does not list. `PRAGMA quick_check(1)` reports a page that two parents reference, where a plain scan of that table returns rows and no error. `PRAGMA hard_heap_limit` turns a schema too large for it into an out-of-memory error, `SQLITE_LIMIT_SCHEMA` refuses a file with more schema objects than it allows, and a progress handler interrupts `quick_check`. `rusqlite` 0.41.1 offers the limits and the authorizer and progress handler without `unsafe`, under its features `limits` and `hooks`, which add no crate. | verified by a probe outside the repository on SQLite 3.54.1 as another build than the bundled one (macOS), and by reading `rusqlite` 0.41.1 and the bundled build's flags (it does not turn memory statistics off, which the heap limit needs). The revised report of deliverable C then ran these of its points on the bundled build with the five features of decision 19, on macOS and in Linux containers of two architectures: the run-time limits, the authorizer, a progress handler that interrupts `quick_check`, the untrusted-schema settings, the immutable read-only open beside planted log and journal files, and the heap limit. That revision is not yet delivered or independently reviewed. R113's tests repeat each point on the bundled build |
| F14 | 1.0 makes a task id in one place, for a mirrored task too: `tsk_` and seven characters drawn from 64 (`A-Z a-z 0-9 _ -`; `crates/orch-engine/src/task_service.rs`, `crates/orch-infra/src/ids.rs`). That id names the task's file `tasks/<id>.yml`, its workspace `workspaces/<id>` and its branch `orchestry/<id>/<slug>`; preparing a workspace reuses a directory that already answers to the name, and cleaning up removes the directory and deletes the branch by name (`crates/orch-infra/src/workspace/manager.rs`). On a filesystem that folds case, two ids that differ only in the case of ASCII letters are one file, one directory and one loose git ref: a second exclusive create of the directory fails, a listing returns the one name as it was created, and `git branch -D` given the other spelling deletes the branch, while a ref that is packed answers only to its own spelling. SQLite's `NOCASE` collation folds exactly the 26 ASCII letters. Two generated ids fold together with probability (116/4096)^7, about 1.5 in 10^11 for a pair and 7 in 100,000 for a project of 3,000 tasks; a 1.0 project on a folding filesystem cannot hold such a pair at all, since the two are one task file. | verified in the code at `fb6a1e1`; the filesystem, git and SQLite behaviour by a probe outside the repository, on macOS (a volume that folds case) and in a Linux container (one that does not, and keeps the two directories and the two refs apart) |
| F15 | 1.0's startup recovery keeps two kinds of running entry and ends the rest (`crates/orch-engine/src/orchestrator/recovery.rs`, `cleanup_stale_running_entries`): a local entry whose pid is alive, and a remote entry whose peer alias is still in the peer registry, or whose registry cannot be read. A 1.0 stop makes such entries: it detaches a remote execution and leaves it running on its peer, and it reports a local process it could not verify and stop. A running entry holds a pid and no process start time (`crates/orch-domain/src/state.rs`, `RunningEntry`). Read at `fb6a1e1`. |
| F16 | In 1.0 a list returns the entity objects cached in `_index.json` and a get reads the entity's own file; a save writes the file first and the index second, so a stop between the two leaves them differing in content. A missing, non-array or corrupt index is rebuilt from the entity files, and an entity file that cannot be read fails the rebuild (`crates/orch-infra/src/storage/index.rs`). Read at `fb6a1e1`. |
| F17 | 1.0 starts every agent process as the leader of a new process group (`Command::process_group(0)` in `spawn_with`, `crates/orch-infra/src/process/manager.rs`), so the group's id is the pid that a running entry records. `is_alive` asks about that pid alone (`kill(pid, 0)`, with `EPERM` read as alive). `kill` signals the group first and the pid second. `kill_with_grace` waits on the pid alone and sends the group `SIGKILL` only while the leader still lives, so a child that outlives a leader ended by `SIGTERM` is left running in the group. `signal_target` refuses a pid of 0 and one above `i32::MAX`, because a zero or a negative target names a group or every process. Read at `fb6a1e1`. |
| F18 | 1.0 has two readers of `config.yml` (`crates/orch-infra/src/storage/config.rs`). The effective one, `FileConfigStore::read_merged`, is what the daemon runs on: it returns 1.0's defaults for a missing file and for any value that is not a mapping, and merges a mapping over the defaults key by key. The guarded reader under it (F12) refuses a stream of two documents and reads a stream of none (an empty file, spaces and newlines only, comments only) as null, so its rule is at most one document and not exactly one. An empty `config.yml`, one that is `null` or `~`, and one that is a scalar or a sequence therefore all run a 1.0 project on the defaults, and 1.0 ships a test of the empty file (`an_empty_file_reads_as_the_defaults`). The authoring reader, `read_raw`, refuses a `config.yml` that is not a mapping ("config: expected an object"), so 1.0 does not agree with itself. 1.0's typed reader refuses a task, an agent, a goal and a team read from a stream of no document or from a null document. `orch init` writes the whole default configuration as a mapping (`crates/orch-daemon/src/bootstrap.rs`), so a `config.yml` that 1.0 wrote sets every key; 1.0's default for `scheduling.max_concurrent_agents` is 6 and it caps local runs (`crates/orch-domain/src/config.rs`, `crates/orch-engine/src/orchestrator/dispatch.rs`). | verified in the code at `54b5790`, where `config.rs`, `definition_yaml.rs` and the lines of `bootstrap.rs` and of the default are as in v1.0.0; the entity refusals, and that 1.0 reads a bare `---` as null, by a probe that is not in the repository, macOS. Not traced: every other 1.0 path that could write a `config.yml` that is not a mapping (`write_raw` validates an object and `write_merged` serialises one) |
| F19 | 1.0's index reader (`IndexManager::read_index`, `crates/orch-infra/src/storage/index.rs`) uses `_index.json` only when the whole file decodes as an array of the directory's entity type (a task, an agent, a goal, a message, a context entry), and otherwise rebuilds it from the entity files and writes it back. Each of the five types has required fields beyond its identity, so one entry that lacks any of them is enough for a rebuild. An array with an entry that is not an object, has no identity or has an identity that is not a string is therefore rebuilt, exactly as an index that does not parse is (F16), and nothing is hidden. An array that holds one identity twice has two cases in 1.0. Where every entry is a complete entity of the type, the array is used as it stands: a list serves that entry twice and hides every file the index does not name, until that id is next saved. Where any entry is not, as in `[{"id":"b"},{"id":"b"}]`, the array is rebuilt like any other that does not decode: the repeat is gone and nothing is hidden. No 1.0 writer produces a repeated identity: a save removes every entry of the id before it appends one, and every other writer filters entries out or writes a rebuilt list. | `read_index` verified in the code at `54b5790`, where `index.rs` is as in v1.0.0, and the required fields in the five type definitions at v1.0.0. The compatibility tests of issue 146 (`import::tests::compatibility` in `orch-infra`, on `main` since 2026-11-22) run the real reader with a test type of one field, `id`, and with no entity type: they pin its two branches, with `[{"id":"b"},{"id":"b"}]` kept because that test type asks for nothing more, and they pin neither case of a repeated identity in a real directory. Both cases were run once on the real reader for a task and for a context entry, and the decoding of an array of identities alone for each of the five types, by a probe that is not in the repository (Linux, at `54b5790`); vector X-V5 makes them tests. Corrected on 2026-11-22: this row said that the kept arrays were pinned against the real reader, without saying that the type was a test type. The writers were read at v1.0.0 (16 call sites) for the rulings this revision follows and not read again |

## Requirements

Test names are the names the implementing PR creates. A PR may choose another name only by editing this table in
the same PR. "Both" meant macOS and Linux CI; since the owner's ruling of 2026-11-23 (record K) it means Linux CI, and
where a row names macOS, Darwin, APFS or a Mac as the place of a check still to be run, that part is withdrawn and
the Linux part stands. Requirements marked "(O*n*)" or "(AP*n*)" rest on that point.
Five points added on 2026-11-22 are marked "(T21-*x*)" and are not answered: the rows that rest on one say
that they are drafted from its default or, for T21-O1, from a proposal (record J of "Owner's answers").
The owner has answered each of the others; "Owner's answers" says what the owner still holds, which is the
numbers and
results of AP13 and the fifth point of AP14, and names a third thing that lies outside the answers: the
owner's word on using the live copy for plan task T4c (AP8). The documents of the other entry steps named here
(`docs/storage-v1-conformance.md`, `docs/storage-order-dependent-reads.md`, `docs/sqlite-library.md`,
`docs/sqlite-event-throughput.md` and the lease prototype's report) arrive in their own PRs; until those merge,
their names are forward references. Since the owner's decision of 2026-11-21 (record I of "Owner's answers") the
numbers and results of AP13 are deferred with the measurement and nothing in 1.1 waits for them, and
`docs/sqlite-event-throughput.md` is not delivered in 1.1: deliverable D is dropped, and
[issue 98](https://github.com/yuxhuang/r-orch/issues/98) links the report and its results at the revision that
holds them.

| Id | Requirement | Source | Priority | Acceptance check (test or command; platform) |
|---|---|---|---|---|
| R1 | The spec, the plan and ADR 0017 shall be in the repository in one documentation-only PR before any product code. | A | must | The three files exist on `main`; `./xtask/gate.sh fast --base origin/main` classifies the PR as documentation only; macOS |
| R2 | A storage conformance suite shall run every scenario against a backend chosen by the test, and shall pass on the file backend before the SQLite backend exists. | B | must | `cargo test -p orch-engine --test storage_conformance`; both |
| R3 | The list of every read whose result depends on index order shall be in the repository, each entry naming the file, the function and what the port must keep for it. | B | must | `docs/storage-order-dependent-reads.md` exists, beside the pinned 1.0 contracts in `docs/storage-v1-conformance.md`; the order scenarios of the suite pass, among them `storage_conformance::equal_score_selections_rotate_after_run_lifecycle_saves` and `::lists_return_all_records_beyond_a_future_default_page`; a reviewer checks the list against the store list calls at the audited commit; both |
| R4 | The SQLite library shall be chosen with the ARCHITECTURE §4.8 check (maintenance, MSRV of the pinned toolchain, licence through `deny.toml`), recorded in ADR 0017 decision 19 from the report `docs/sqlite-library.md`, bundle SQLite 3.37 or later with FTS5 available, and leave `unsafe_code` forbidden in the workspace. | C | must | The report and its probes under `docs/experiments/sqlite-library/`; `cargo deny check` passes on the PR that adds the dependency; `crates/orch-infra/tests/sqlite_build.rs::bundled_sqlite_has_strict_tables_and_fts5`; both |
| R5 | — | D, G7; the owner's decision of 2026-11-21 | won't | Deferred by the owner on 2026-11-21 (record I of "Owner's answers"): not in release 1.1. No measurement of event throughput or of claim latency is required for 1.1, and none is a condition of a PR or of the gate. The first run is not repeated, replaced or pooled: its budget, figures and FAIL are kept in [issue 98](https://github.com/yuxhuang/r-orch/issues/98), which links the report and the raw results at the revision that holds them, and that report is not delivered to `main` in 1.1. As it stood until then, kept as a record and not in force: "Batched event throughput shall be measured against a claim-latency budget declared before the run ("Writer lanes"), on macOS and on Linux, with the commit, the hardware class and the raw figures recorded. The first run and its verdict against the first budget shall be kept unchanged beside every later run." Its check was: The report `docs/sqlite-event-throughput.md` and its experiment under `docs/experiments/sqlite-events/`, presented at the approval gate; macOS and native Linux |
| R6 | A prototype shall show a claim with a fenced lease, including a suspended and resumed machine, and its result shall confirm or replace ADR 0017 decision 16 and R39 before the engine PR starts. | E | must | The prototype's report and its tests under `prototypes/fenced-lease/`, presented at the approval gate. The prototype models a suspend as elapsed time that includes it, and a real suspend is not part of its acceptance: the real-suspend check is R42's, on Linux, and pending (issue 110). Linux: the 21 tests of `prototypes/fenced-lease` passed on Linux x86_64 at `200da1d4` (the first independent review's run, `cargo test --locked` in that directory; the prototype is not a workspace member, so CI never runs it). The prototype's recorded passes on macOS arm64 are history |
| R7 | When the work of a unit of work returns an error or the process stops before the commit, the storage shall keep none of its changes. | F, B3 | must | Bare filters `unit_of_work_keeps_nothing_after_an_error` and `a_failed_unit_of_work_keeps_no_setting_or_template_row_or_position`, each selecting the 3 entity references `memory_v2`, `queued_memory_v2` and `file_v2` (the plan's "T8 consumer contracts (issue 109)"; `cargo test -p orch-engine --test storage_conformance -- <filter>`); `cargo test -p orch-infra --test sqlite_crash -- kill_before_commit_keeps_nothing`, the production writer's process-crash boundary. SQLite shared registration at the port: T10; Linux |
| R8 | Every entity shall carry a revision that is 1 at creation and goes up by one on every update, and an update or delete whose expected revision is not the stored one shall fail with `StaleRevision` and change nothing. | F, B3 | must | `revisions_and_positions_for_every_entity_family`, `stale_revision_is_refused_and_changes_nothing`; entity references (see plan crosswalk); Linux |
| R9 | A list shall return at most 100 rows by default and at most 1,000 on request, shall refuse a larger limit, shall return a cursor while more rows remain, and shall count the matching rows only when asked. | F | must | `page_bounds_defaults_and_optional_count`; entity references; Linux |
| R10 | A walk through a list by cursor shall return every row whose sort values did not change during the walk exactly once, whatever is inserted or deleted meanwhile, and a cursor used with another filter or sort shall be refused. | F | must | `cursor_walk_is_stable_under_inserts_and_deletes`, `cursor_is_bound_to_project_filter_and_sort`; entity references; Linux |
| R11 | Creating an entity shall give it the next `position` of its project and table, an update shall leave `position` unchanged, and `move_to_end` shall give it the next `position`. | F, B3 | must | `revisions_and_positions_for_every_entity_family`, `deletion_never_reuses_a_position`; entity references; Linux |
| R12 | When several callers claim a lease for one task at once, exactly one shall be granted and the others shall be told who holds it; no agent shall hold two active leases. | F, B9 | must | `one_of_many_concurrent_claims_wins`, `an_agent_holds_at_most_one_active_lease`; memory and file lease references; SQLite shared registration: T10; Linux |
| R13 | Every lease shall carry an epoch larger than every epoch issued before for that task, and a renew, release, event append or finish that names an older epoch shall be refused and change nothing. | F, S1 | must | `epochs_only_grow`, `a_stale_epoch_is_refused`, `release_ends_the_lease_and_keeps_the_epoch_counter`; memory and file lease references; `stale_lineage_epoch_missing_lease_and_task_mismatch_change_nothing` on memory and file event references covers append. Finish/restart: T12; SQLite shared registration: T10; Linux |
| R14 | An event append shall store a batch in one transaction and skip every sequence number already stored for that run, reporting how many it stored and how many it skipped. | F, M | must | `duplicate_counts_first_bytes_and_range_tail_order`, `transaction_error_rolls_back_append_gap_prune_and_entities`; memory and file event references; SQLite shared registration: T10/T16; Linux |
| R15 | The port shall be entered only through `Storage::read` and `Storage::write`, each taking a caller (principal and project), and a call shall see and change only rows of the caller's project. | F, B4 | must | `caller_selects_the_project_and_keeps_the_principal`, `settings_and_templates_belong_to_their_project`; entity references; project is taken from the caller, rather than a cross-project write argument being refused; Linux |
| R16 | After the engine PR, `orch-core` shall export no whole-document state store, no merge-key rule and no implicit reorder on save, and `docs/TRAITS-FROZEN.md` §2 shall describe port v2. | B3 | must | `git grep -nE 'trait StateStore\|apply_merge' crates/orch-core crates/orch-engine/src` prints nothing; reviewer check of TRAITS-FROZEN §2 against the port; Linux |
| R17 | The file backend shall implement port v2, so that the conformance suite runs whole on both backends; after the removal PR it shall live in a crate that only dev-dependencies may name, for one release. Atomicity across a process stop is required of SQLite only. | B1 | must | `cargo xtask crate-graph` fails when a product crate depends on it (`xtask` test `file_reference_is_dev_only`); both |
| R18 | A new database shall have exactly the tables listed in "Schema for 1.1", all `STRICT`, with `project_id` on every table outside the daemon-wide group and in the primary key wherever the sketch has it there. | G, B4, O2 | must | `crates/orch-infra/tests/sqlite_schema.rs::new_database_matches_the_schema_snapshot`, `::every_project_table_carries_project_id`; both |
| R19 | The schema shall contain, from the first migration, every column named in "Columns kept for later phases". | G, O2 | must | `sqlite_schema::kept_columns_are_present`; both |
| R20 | No 1.1 operation shall put a task in `proposed`, `landing` or `needs_attention`, and every task that becomes `done` shall get `landing_state = 'skipped'`. | B13 | must | `crates/orch-engine/tests/task_statuses.rs::no_operation_enters_a_1_5_status`, `::done_is_recorded_as_skipped`, `sqlite_schema::done_without_landing_state_is_rejected`; both |
| R21 | Migrations shall be numbered, applied in order inside one transaction each, recorded in `schema_migrations`, and never reversed; a failed migration shall leave the database at the earlier version and stop the start. Before a pending migration the daemon shall take the copy of R124. | G | must | `crates/orch-infra/tests/sqlite_migrations.rs::migrations_apply_in_order_and_are_recorded`, `::failed_migration_leaves_the_earlier_version`; both |
| R22 | When the database's schema version is newer than the binary's, the daemon and every offline command shall refuse to open it and name both versions. | G | must | `sqlite_migrations::newer_schema_is_refused_with_both_versions`; both |
| R23 | Every connection shall run with WAL journal, `synchronous = NORMAL` unless `storage.synchronous` is `full`, foreign keys on and a busy timeout of 5,000 ms. | B12 | must | `crates/orch-infra/tests/sqlite_connection.rs::connections_have_the_required_settings`, `::synchronous_full_is_a_setting`; both |
| R24 | Exactly one connection shall write, every write shall start with `BEGIN IMMEDIATE`, and every other connection shall be read-only. | B6, B12 | must | `sqlite_connection::a_read_connection_refuses_writes`, `crates/orch-infra/tests/sqlite_writer.rs::writes_run_one_at_a_time_on_one_connection`; both |
| R25 | A read shall see one consistent snapshot from its first statement to its last, whatever commits meanwhile. | B6 | must | `sqlite_writer::a_read_snapshot_does_not_see_a_later_commit`; both |
| R26 | The writer shall always take a queued claim or lease transaction first, and otherwise the queued transaction with the earliest deadline, where the deadline is arrival plus 200 ms for entity changes, 1 s for events and 5 s for maintenance; a bulk job shall have at most one transaction queued at a time, and the event lane at most one for all runs together, holding the batches that wait, up to R27's bound. A claim shall therefore wait behind at most the one transaction in progress. That is an order and not a time: the waits of "Writer lanes" are targets that no check of 1.1 measures (the measurement of R5 is deferred: record I of "Owner's answers"), and a test with a fake clock proves the order only. | G; red team W1, W4 | must | `sqlite_writer::a_claim_runs_before_everything_queued` (fake clock), `::earliest_deadline_runs_first_among_the_other_lanes`, `::overdue_maintenance_does_not_delay_a_claim`, `::no_lane_starves_under_a_flood_of_another`, `::waiting_event_batches_share_one_transaction`; both |
| R27 | Bulk work (event batches, pruning, expiry, import) shall be cut into transactions of at most 1,000 rows or 20 ms of work; an atomic operation over that bound shall run whole and be counted in `writer_txn_over_bound_total`. (AP8) | G | must | `sqlite_writer::bulk_work_is_cut_at_1000_rows`, `::an_atomic_operation_over_the_bound_runs_whole_and_is_counted`; both |
| R28 | A side effect outside the database shall be written as an `intents` row in the transaction that decides it, performed after the commit, and marked done; an intent left by a stopped daemon shall be handled at the next start as its kind states in "Transactions and intents". | G, H | must | `crates/orch-engine/tests/intents.rs::intent_is_written_with_its_change`, `::pending_intent_is_handled_after_restart`, `::a_repeated_intent_has_no_second_effect`; both |
| R29 | The data directory shall have the layout and modes in "Data directory"; the daemon shall create it with those modes and refuse to start when it exists with group or other access or another owner. | B5 | must | `crates/orch-daemon/tests/daemon_data_dir.rs::layout_and_modes_are_as_specified`, `::a_directory_open_to_others_is_refused`; both |
| R30 | When a daemon or an offline command is pointed at a data directory that another holds, it shall change nothing, exit 4 and print the holder's kind, pid and start time. | B5, G6 | must | `crates/orch-daemon/tests/daemon_data_dir.rs::second_daemon_refuses_and_names_the_holder`, `crates/orch-cli/tests/db_offline.rs::offline_command_refuses_while_a_daemon_holds_the_directory`; both |
| R31 | While a 1.1 daemon or `orch import` runs for a project directory, a 1.0 daemon shall not be able to start on it, and a 1.1 daemon shall refuse to start while a 1.0 daemon holds it; neither shall create or write a 1.0 file. Where the 1.0 lock file does not exist (no 1.0 daemon ever ran there), or if F9 proves false, 1.1 shall only refuse while a 1.0 daemon answers, and the manual shall say that the exclusion is then a procedure and not a lock. | B2, F9 | must | `crates/orch-daemon/tests/daemon_legacy_lock.rs::legacy_owner_lock_is_held_without_writing`, `::start_is_refused_while_a_legacy_daemon_holds_the_project`, `::an_absent_legacy_lock_file_is_not_created`; Linux |
| R32 | The daemon shall refuse to start when the data directory is on a network filesystem. | B12 | must | `daemon_data_dir::network_filesystem_is_refused` (injected filesystem type); both |
| R33 | The data directory shall be `ORCH_DATA_DIR` when set (absolute, else exit 2), otherwise the directory that the project's record under `<data home>/projects/` names (R107), which `orch init` and `orch import` write when they create `<data home>/<name>` (an import from an export file writes it when its database is published, R115); the data home is `ORCH_DATA_HOME` or `~/.orchestry/daemons`; the repository's cargo configuration shall set `ORCH_DATA_HOME` inside `target/`. (O1) | B5, DL | must | `daemon_data_dir::default_name_is_derived_from_the_project_directory`, `::the_project_record_names_the_directory`, `::relative_override_is_refused`; `./xtask/gate.sh pr` `real-home` step unchanged; both |
| R34 | `orch init` and `orch import` shall write `.orch-project` (the project id and a newline) in the project directory; a client shall find the project by `ORCH_SOCKET`, then `ORCH_PROJECT_ROOT`, then the nearest ancestor holding `.orch-project`; a directory with 1.0 state and no `.orch-project` shall get exit 3 and the hint `orch import --dry-run`. When the project directory is not the one the database records, the daemon shall adopt the new path if the recorded one no longer holds that project id (a move) and refuse, naming both paths, if it still does (a copy). (O1) | O1 | must | `crates/orch-cli/tests/project_pointer.rs::client_finds_the_project_by_pointer`, `::unimported_project_gets_the_import_hint`, `::a_moved_project_keeps_its_state`, `::a_copied_project_is_refused_with_both_paths`; both |
| R35 | Starting a run shall be one transaction: the lease, the epoch, the run row, the task's status and attempts, the removal of its retry row and the intent to start the process. | H | must | `crates/orch-engine/tests/transactions.rs::start_run_is_all_or_nothing` (a stop injected before and after each statement); both |
| R36 | Finishing a run shall be one transaction: the run's outcome and usage, its result row, the task's next status, its retry row or its `landing_state`, the release of the lease, the agent's totals and position, and the intents to clean up. A finish shall carry the lineage and the epoch of its lease (R132), and the transaction shall check both. A finish with the current lineage and a stale epoch shall change only that run's row (its outcome and usage, which deliverable Q wants for every run); in 1.1 the only caller of a finish is the daemon's own supervisor of that run. A finish with an earlier lineage shall be refused as stale and shall change no row of any table, that run's row included. A run of an earlier lineage that a restore brought back is closed by the daemon's own recovery under the new lineage (R41, R100), which is not a caller's path. | H; the security re-review's condition C1 (S6: the earlier text let an earlier-lineage finish change its run's row while R132 refused it) | must | `transactions::finish_run_is_all_or_nothing`, `::finish_with_a_stale_epoch_changes_only_the_run`, `::finish_with_an_earlier_lineage_changes_no_row` (after a restore and again after a restart of the daemon: every row of every table, the daemon-wide ones included, is read before and after the refused finish and is identical, and no spool file is written for it); both |
| R37 | Cancelling a task shall be one transaction that also ends its run, releases its lease, removes its retry row and writes the intent to stop its process; the process shall be stopped after the commit and again after a restart if it still runs. | H | must | `transactions::cancel_task_is_all_or_nothing`, `intents::stop_intent_is_repeated_until_the_process_is_gone`; both |
| R38 | Pausing a goal shall be one transaction with the cancellations it implies, and no claim for a task it cancelled shall commit after it. | H | must | `transactions::pause_goal_is_atomic_with_dispatch` (claims and pause raced 1,000 times); both |
| R39 | A permanent failure shall be one transaction with its whole cascade. | H | must | `transactions::permanent_failure_and_its_cascade_are_one_transaction`, `::a_cascade_over_1000_rows_is_still_one_transaction`; both |
| R40 | Leases and the retry table shall replace `state.json`; a retry shall never be dropped for lack of room. (AP10) | H | must | `crates/orch-engine/tests/leases.rs::running_and_claimed_are_leases`, `crates/orch-engine/tests/retries.rs::the_101st_retry_is_kept`; both |
| R41 | At every start the daemon shall take a new generation and treat every lease of an earlier generation as 1.0's startup recovery treats a running entry (`docs/internal/orchestration.md`, "Startup recovery"): a run whose recorded process is gone is ended and its task handled as 1.0 handles it; a run whose recorded process is alive keeps its lease until the process exits; a run still `preparing` is ended. An unfinished run that holds no lease (an export carries none, R105) shall be ended, and its task handled, as a run whose process is gone, without reading its recorded pid. Before it judges a run it shall apply that run's spooled finish (R129), by R36's rule for lineage and epoch. It shall never start a process for an intent of an earlier generation. | H, G9; security review S2 | must | `leases::recovery_matches_1_0_for_dead_alive_and_preparing_runs`, `::an_unfinished_run_without_a_lease_is_ended_without_a_signal`, `::a_spooled_finish_is_applied_before_recovery_judges_the_run`, `intents::a_start_intent_of_an_earlier_generation_is_not_performed`; both |
| R42 | A lease shall not be taken from a live local run because the machine was suspended: after a resume the daemon shall renew the leases of processes it still supervises before it expires any. (F10; AP12, which the owner answered as recommended) | E | must | `leases::a_clock_jump_does_not_expire_a_supervised_run` (clock seam; the clock is injected, so it proves renewal ordering only); Linux. And a recorded check on Linux: a supervised run in flight through a real suspend and resume of an isolated Linux host that the owner has authorized, noting which clock the product reads (issue 110; D6). That check is pending: nothing has been recorded and no host has been suspended. The check that this row once held on an isolated macOS machine is withdrawn by record K; a physical macOS observation is deferred to the After 2.0 milestone (issue 178) |
| R43 | The 1.1 maintenance jobs in "Maintenance jobs" shall run from one timer with a due time per job, stored so that a restart neither skips a due job nor runs every job at once. (O5) | H, O5 | must | `crates/orch-engine/tests/maintenance.rs::each_job_runs_when_due`, `::due_times_survive_a_restart`; both |
| R44 | The daemon shall start with dispatch as it was when it last stopped, and off after an import, a restore or a start that applied a migration; `--dispatch on\|off` shall override; the start shall print the state and its reason. (O4) | O4 | must | `crates/orch-daemon/tests/daemon_dispatch.rs::dispatch_resumes_as_it_was`, `::dispatch_is_off_after_import_restore_and_migration`; both |
| R45 | When a mutate or destructive request whose change is a database transaction carries a key already stored for its scope with the same request hash, the daemon shall make no change and return the first result marked as a replay. | I, G8 | must | `crates/orch-daemon/tests/ops_request_keys.rs::a_repeated_change_runs_once`, `::a_reply_lost_before_delivery_is_replayed`; both |
| R46 | When a request carries a key already stored for its scope with a different request hash, the daemon shall refuse it with `conflict`, execution `not_executed`. | I, G8 | must | `ops_request_keys::same_key_with_a_different_body_is_refused`; both |
| R47 | A key shall be stored in the same transaction as its change, kept for one hour and then deleted by the `request_keys.expire` job; a result over 64 KiB shall not be kept, and its replay shall say so. | I, O8 | must | `ops_request_keys::key_and_change_commit_together`, `::a_key_expires_after_an_hour`, `::a_result_over_64_kib_is_not_kept`; both |
| R48 | Every entity returned by the API shall carry its `revision`; a change that names `expected_revision` shall fail with `stale_revision` when it is not the stored one; a change that names none shall apply as in 1.0. | I | must | `crates/orch-daemon/tests/ops_revisions.rs::entities_carry_a_revision`, `::stale_expected_revision_is_refused`, `::a_change_without_one_applies`; both |
| R49 | Every list operation shall accept `limit`, `cursor`, `sort` and `with_count` with the bounds of R9. CLI list commands shall return all rows in their 1.0 shape and order unless `--limit` or `--cursor` is given: they walk the pages by the immutable sort `id`, so that every row present throughout the walk is returned exactly once, and order the result as 1.0 did. (AP4) | I | must | `crates/orch-daemon/tests/ops_paging.rs::list_operations_page`, `crates/orch-cli/tests/list_paging.rs::default_output_is_unchanged`, `::a_row_changed_during_the_walk_is_returned_once`, `::limit_returns_one_page_and_a_cursor`; both |
| R50 | `LOCAL_PROTOCOL` shall be 3, and a client and a daemon of different protocol versions shall refuse each other as in 1.0, with the frozen handshake and the stop intent unchanged. | I | must | `crates/orch-proto` test `protocol_is_3`, `crates/orch-daemon/tests/daemon_wire.rs::protocol_2_client_is_refused_by_a_protocol_3_daemon`, `::stop_intent_crosses_the_skew`; both |
| R51 | The error codes `stale_revision`, `quota_exceeded`, `storage_unavailable` (R128) and `snapshot_expired` (R127) shall be added with exit 1; the twelve existing codes and exits 1 to 7 shall keep their meanings. | I | must | `crates/orch-proto` test `existing_error_codes_and_exits_are_unchanged`, `::new_codes_exit_1`; both |
| R52 | The dispatcher shall decide whether a principal may run an operation in one function with an exhaustive match and no default arm, before any admission or storage call. | J | must | `crates/orch-daemon/tests/ops_authorization.rs::every_operation_has_a_row_for_every_principal_kind`; a reviewer confirms no other check site; both |
| R53 | A test principal of kind worker and one of kind task shall be refused every operation the matrix denies them, with `not_permitted` and no change, including any call for another project and any lease-bound call with a stale epoch. | J, G5 | must | `ops_authorization::worker_is_refused_what_the_matrix_denies`, `::task_is_refused_what_the_matrix_denies`, `::another_project_is_refused`; both |
| R54 | Every `db.*` operation, `attention.ack`, `policy.set`, `orch import`, `orch db restore` and `orch db vacuum` shall be refused over anything but the local socket or the local lock, to an MCP session of any mode, and in agent context. (AP3) | B8, B14 | must | `ops_authorization::owner_only_operations_are_refused_to_mcp_and_agent_context`, `crates/orch-cli/tests/db_offline.rs::offline_commands_refuse_in_agent_context`; both |
| R55 | The scheduler shall be an interface that receives a view of open tasks, agents and leases and returns task-to-agent pairs; it shall not write, and a pair that loses its claim shall start no run. | K, B9 | must | `crates/orch-engine/tests/scheduler.rs::scheduler_has_no_write_access`, `::a_raced_assignment_starts_one_run`; both |
| R56 | The 1.1 scheduler shall choose the same task and agent as 1.0 for every scenario of the order list, including the rotation of tied agents. | K, B9, G1 | must | `crates/orch-engine/tests/order_conformance.rs` on both backends (`::tied_agents_rotate`, `::dispatch_order_matches_1_0`); both |
| R57 | The daemon shall record the duration of every scheduler tick and the number of open tasks by status, and return them from `metrics.get`. | K | must | `crates/orch-daemon/tests/ops_metrics.rs::tick_duration_and_open_tasks_are_reported`; both |
| R58 | The blob store shall write a blob to `blobs/tmp/`, sync it, rename it to `blobs/sha256/ab/cd/<hash>` and only then let a row name it; a read shall fail when the bytes do not hash to the name. | L, B7 | must | `crates/orch-infra/tests/blob_store.rs::bytes_exist_before_the_row`, `::a_corrupted_blob_is_detected_on_read`; both |
| R59 | A rendered prompt shall be stored once per content hash and named by `runs.prompt_sha256`; a task attachment shall be a blob named by `task_attachments.sha256`. | L | must | `blob_store::equal_prompts_are_stored_once`, `crates/orch-engine/tests/attachments.rs::attachment_is_a_blob`; both |
| R60 | The `blobs.sweep` job shall delete a blob only when no row has named it for 7 days, and shall remove files in `blobs/tmp/` older than 24 hours. | L | must | `maintenance::unreferenced_blob_is_deleted_after_seven_days`, `::a_referenced_blob_is_never_deleted`; both |
| R61 | Run events shall be stored with a per-run sequence number from 1, read by range and by tail from an index, and a batch the daemon had to drop shall leave a row in `run_event_gaps`. The daemon shall give an event its number when a collector hands it over, from a counter kept for each run, and never at the commit, so that events handed over and not committed have numbers that a gap row can name (R139). When the daemon starts, a run's counter shall continue one above the greatest of: the highest number committed for that run, as an event or as the last number of a gap row; and the highest number recorded for that run in `spool/events-at-stop`. No number of a run that is committed, as an event or inside a gap row, or that is recorded in `spool/events-at-stop` shall be given again. A number that was given and then lost is neither of those: it may be given again to a later event, and no gap row says so. Four such losses are stated in this spec, and the list is not complete: the numbers of events that a daemon held when it was killed; of commits that were lost with the power under `synchronous = NORMAL`; of events that a stop could neither commit nor record, because the write of its record failed as well ("Stopping"); and of a stop whose record was lost with the power or failed its check ("What a sync promises"). This requirement adds no durable record of the numbers given. That loss is the one that "Stopping" states for a killed daemon. It stays open as an item of its own, first named by the resilience review of `3c50293` as its N10: it is part of no other review's item, and closing another item does not close it. | M; the resilience review of `a914e78`, N4 (R139 records a range of numbers, and nothing said that events have numbers before they commit); the resilience review of `3c50293`, N10 ("no number of a run shall be given twice" did not hold for numbers lost with a kill or with the power); the resilience review of `d2fe9b0`, N14 and N15 (the loss was counted with an item of another review that is about something else; the losses named could be read as all of them) | must | `crates/orch-engine/tests/run_events.rs::events_are_numbered_from_one`, `::tail_read_returns_the_last_n`, `::a_dropped_batch_leaves_a_gap`, `::an_event_has_its_number_at_handover`, `::numbering_continues_above_the_committed_and_the_recorded_numbers_after_a_start`; both |
| R62 | The `events.prune` job shall delete the events of a run that finished successfully more than `retention.events.success_days` (90) ago or otherwise more than `retention.events.failed_days` (365) ago, shall skip a class set to `never`, shall record `events_pruned_at` and `events_pruned_count` on the run, and shall never delete a run, a result or a blob named by one. | M, B10 | must | `maintenance::events_are_pruned_by_outcome_and_age`, `::never_disables_pruning`, `::results_survive_pruning`, `::an_interrupted_prune_resumes`; both |
| R63 | Run events shall be stored in SQLite in release 1.1, as rows of `run_events` and `run_event_gaps`, by the batched design of "Writer lanes": a collector hands a run's batch over at 256 events or when its oldest event is 250 ms old; the batches that wait are committed together, in the one transaction that the event lane has queued for all runs (R26), cut at R27's bound; and a collector whose buffer is full drops the oldest raw output and leaves a gap row (R61). 1.1 shall build no event fallback: no event file, no `run_event_batches` table and no setting that chooses another event path. No latency or throughput figure shall be a condition of storing events in SQLite, of a PR or of the gate. This is the owner's decision of 2026-11-21 (record I of "Owner's answers"). It defers the rest of this requirement as it stood until then: the measurement that was to precede storing events in SQLite, the selection of a path from its misses, and the measurement of the shipped path as built. That text is kept below as a record. It is not in force in 1.1, and nothing is built, run or held back on account of it. The owner's ruling and the answers in it (records E, F and H) are not withdrawn, and they have nothing to act on until latency is taken up again. **As it stood until 2026-11-21, deferred:** Events shall be stored in SQLite only when the revised measurement of "Event measurement" has passed on macOS and on native Linux hardware and the owner has accepted its budget and its results. Every miss of the revised budget shall be kept and reported, whatever follows from it. A miss shall select the event fallback of "Writer lanes", behind the same port operations, only when it is attributable to events: a miss in a population that holds no events (cell C0, or the claims of a burst alone) and a miss that only one platform shows shall select no event path and go back to the owner, the second even when its population holds events ("Event measurement", "What a miss selects"). The two sentences before this one state the owner's ruling of 2026-11-21: the owner answered "1." to the option that keeps every miss, selects the fallback only for a miss that is attributable to events, and sends a miss without events or on one platform alone back to the owner (record E of "Owner's answers"). The ruling amends the sentence "If the revised budget also fails, the file fallback applies" of the direction of 2026-11-20. Before any of that is read, a data set shall be eligible to select a path: no path, SQLite as the candidate included, shall be selected from a data set in which any repetition, mandatory or diagnostic, has a correctness defect or a correctness that is unknown, in which a required repetition is invalid or does not show its backend, its settings, its timing check or its platform class, or whose coverage is incomplete; every miss and every axis of such a data set shall be reported all the same, under the headline and the exit that "The verdict" gives it. This eligibility rule is this spec's proposal, which the method review asked for. It is not in the ruling's words, and the owner has not ruled on it. By the owner's two answers of 2026-11-21 that followed it (record F of "Owner's answers"): a miss shall count as shown by both platforms only when each has a valid mandatory repetition of the same cell that missed the same kind of limit in the same population, so that two different kinds of miss in one cell, one on each platform, shall select no path and go back to the owner; and after a data set in which a mandatory repetition that is `valid` or `host_not_idle` has missed, no further data set shall be started without the owner's word, whatever its headline, a data set that only fixes a defect included. By four answers of 2026-11-21 that were given under the owner's delegation (record H of "Owner's answers"): the kinds by which that judgment is made shall be the five of "What a miss selects" (the regular claims, the claims of a checkpoint window, the claims of a burst window, an event's age, and a refusal or something still queued), and the match shall be made on the cell, the population and the kind together, by whatever computes the selection and not in a report alone; a miss that step 2, 3 or 4 of "What a miss selects" covers, anywhere in a data set, shall keep that data set from selecting an event path, and an eligible data set in which every miss is a valid miss in a population that holds events, shown by both platforms in the same cell, population and kind, shall still select the fallback; and starting the second platform's block after the first platform's block has a mandatory miss in a repetition that is `valid` or `host_not_idle`, and replacing an `invalid` or `host_not_idle` set of five while the data set holds such a miss, shall each be further measurement that is not started without the owner's word, with the replaced set and every miss kept. A path that is selected is not by that permitted to be measured and is not by that qualified to ship. The first run's budget, samples, figures and FAIL verdict shall be kept as they are: no later figure shall replace them, be pooled with them or be reported without them. Every repetition that counts shall contain completed checkpoints by the production rule (R126) while traffic runs, with the claims around each checkpoint reported apart; a run without them is incomplete and never a pass. Each data set shall get its verdict by the one rule of "Event measurement" ("The verdict"), with its axes reported apart, and a repetition measured on a host that was not idle ("The host") shall never count toward a pass and shall never remove a miss. Whichever path ships shall itself be measured as built (the port, the writer, the checkpoint job and the shipped collector's settings) by the same method and against the same budget, on macOS and on native Linux hardware, before its PR merges; a path that misses goes back to the owner and is not shipped on a restated budget. No number of the revised budget is accepted until the owner says so. (AP13) | M; the owner's decision of 2026-11-21 (record I of "Owner's answers"). As it stood, deferred: G7, D; the owner's direction on AP13 and the owner's ruling of 2026-11-21 on what a miss selects; red team W2, W2-R1, O1, O2 and O5; the method challenge MC1 to MC3 and MC8; the resilience review F4; the method review of `a914e78`, MC3 (the ordered steps reached the fallback from a data set with a defect); the owner's answers of 2026-11-21 on how "one platform alone" is judged and on a further data set after a miss, which the red team's review of `3c50293` asked to be put to the owner (L1, D1); the four answers of 2026-11-21 given under the owner's delegation (record H), on the list of kinds, on the whole data set and on the two questions about further measurement; the facts review of `3c50293`, N4 (the sentence "That is the owner's ruling" stood after the eligibility rule and so named a proposal as the ruling) | must | The event checks of R14, R26, R27, R61, R62 and R139, and the event scenarios of `storage_conformance` on SQLite (G1); both. As it stood until 2026-11-21, deferred and not due in 1.1: The record of R5, with every figure of the first run kept; the record of the revised measurement and of the path that ships (`docs/sqlite-event-throughput.md`, the method and its cells frozen before the run, each repetition's checkpoint calls with their outcomes, its host record and its verdict axes listed); the owner's acceptance of the numbers, recorded in "Owner's answers"; if the fallback ships, the `storage_conformance` event scenarios pass on it; macOS and native Linux hardware |
| R64 | `orch import` shall open every source file read-only and create, change or delete nothing under the source `.orchestry/`. | N, B2 | must | `crates/orch-cli/tests/import.rs::source_tree_is_byte_identical_after_import` (content and metadata snapshot; source made read-only); both |
| R65 | `orch import --dry-run` shall write nothing anywhere and shall report the counts per entity, every file it cannot read, every source entry that a budget of "Import bounds" refuses (with the rule, the limit and what was observed), every task id and workspace name that R117 refuses, every workspace that no task names, every unknown key, every dangling reference, every field over 64 KiB, the policy it would turn on, the in-flight state it would close, every in-flight entry that R119 refuses (`in_flight_live`, `in_flight_remote`), every entity whose index entry differs from its file (`index_differs`), every index it goes on without (`index_unreadable`) and every index that stops it (`index_entry`, `index_duplicate`) (R122), a `config.yml` that it refuses and that 1.0 read as its defaults (R70), every setting that it would import at 1.0's default because `config.yml` does not set it (R70; a note and no problem class; it rests on T21-O1, a proposal), and the space it needs. For each class it shall say whether the import stops there (R123). | N, O7 | must | `import::dry_run_writes_nothing`, `::dry_run_report_lists_every_problem_class` (pathology corpus, with the hostile corpus of "Import bounds"); both |
| R66 | `orch import` shall read its source in bounded memory and bounded transactions, refuse a source file over the byte bounds in "Import bounds" (R109 has the budgets inside a document, R111 the event lines and the counts), and commit an entity and its `import_manifest` row together. | N; security review S1 | must | `import::memory_stays_bounded_on_a_large_project` (generated corpus of the live project's shape), `::oversize_source_file_is_refused_by_name`; both |
| R67 | When an import from a 1.0 directory was stopped part-way, `orch import --resume` shall continue from the manifest, shall refuse when a source file it already imported has changed, and the daemon shall refuse to start on an unfinished import. An import from an export file leaves nothing to resume (R115): `--resume` with an export file shall be refused with exit 2. | N; security review S1 | must | `import::resume_completes_an_interrupted_import` (stops injected at every batch boundary), `::resume_refuses_a_changed_source`, `daemon_data_dir::unfinished_import_is_refused`, `crates/orch-cli/tests/db_export_import.rs::resume_is_refused_for_an_export_file`; both |
| R68 | `orch import` shall refuse, changing nothing, when a 1.0 or 1.1 daemon holds the project or the data directory, when the data directory already holds a finished project, or when free space is under the disk floor plus the estimate. (O3) | N, O3 | must | `import::refuses_while_a_daemon_runs`, `::refuses_a_second_project`, `::refuses_without_space`; both |
| R69 | An imported project shall keep its ids, its order (`position` from each `_index.json`), its retry queue, its totals and its runs; state that was in flight shall be closed as 1.0's reconcile would close it when its process is gone (R105), once R119 has let the import proceed; every task's next epoch shall be above every epoch given to its imported runs. | N | must | `import::order_and_rotation_survive_import` (the order scenarios run on the imported database), `::in_flight_state_is_closed`, `::epochs_continue_after_import`; both |
| R70 | A known `config.yml` key shall be imported to its setting; unknown keys shall be stored in the `legacy.config` object, at most 64 KiB, which the daemon never reads; `scheduling.max_concurrent_agents` shall become the project's optional `run_cap`; a project with an agent whose policy is `auto` shall be imported with `preapproval` on. (O7) A project with no `config.yml` shall be imported as one whose `config.yml` is the empty mapping, and no problem shall be listed for the absence. A `config.yml` whose stream holds no document shall be refused under `documents` (R109), and one whose one document is not a mapping (null, a scalar, a sequence) shall stop the import as a document that does not fit its type; each refusal shall name the file and say that 1.0 read it as its defaults (F18). The two refusals are drafted from the unanswered defaults of T21-C1 and T21-C2. A known key that the mapping does not set shall be imported at the value 1.0 ran the project with, which is 1.0's default for it, so `run_cap` shall be 6 where `scheduling.max_concurrent_agents` is not set, and the dry run shall say that the value is 1.0's default. That sentence is a proposal (T21-O1): the owner has not answered it, and "Import rules" has the alternative and what each does. | N, O7, B14; [issue 146](https://github.com/yuxhuang/r-orch/issues/146); F18 | must | `import::unknown_config_keys_go_to_the_legacy_object`, `::max_concurrent_agents_becomes_run_cap`, `::auto_policy_turns_preapproval_on`, and the vectors C-V1 to C-V14 of "Vectors for `config.yml` and for an index", each with its test; both |
| R71 | The importer shall be verified on a copy of the owner's live project, outside the repository: a dry run, an import stopped and resumed, `orch db verify`, and counts equal to the dry run's; the record shall hold counts, sizes, durations and hashes only. | G2, DL | must | `cargo xtask import-verify --source <copy> --record <dir>`; the owner's machine, with the owner's permission |
| R72 | `orch db status` shall report the path, sizes, schema version, generation, journal and sync modes, free space and floor, row counts, the last run of each maintenance job and the import record. | O | must | `crates/orch-cli/tests/db_commands.rs::status_reports_the_listed_fields`; both |
| R73 | `orch db verify` shall run SQLite's integrity and foreign-key checks and the invariants in "`orch db` commands", check every blob's size and hash with `--blobs`, and exit 1 naming each failure. | O | must | `db_commands::verify_passes_on_a_healthy_database`, `::verify_names_each_planted_fault`; both |
| R74 | `orch db query` shall run one statement that reads only the diagnostic views of "`orch db` commands", on the connection and under the limits of R130, and shall refuse everything else: a table, a schema table or a pragma read directly, an attach or detach, a write, DDL, an extension and a second statement. No view shall show `identities`, `identity_tokens`, `daemon_config`, `request_keys`, the value of `legacy.config` or a blob's bytes. | O; security review S3 | must | `crates/orch-cli/tests/db_query.rs::reads_through_a_view`, `::a_table_read_directly_is_refused` (also inside a subquery, a common table expression and `count(*)`), `::attach_pragma_ddl_dml_and_a_second_statement_are_refused`, `crates/orch-infra/tests/sqlite_schema.rs::every_view_column_is_listed_and_no_secret_column_is`; both |
| R75 | `orch db backup` shall write, while the daemon runs, one directory holding a consistent copy of the database, every blob that copy names, the repositories and a manifest with hashes. The directory shall be one that the command creates exclusively below `backups/` of the data directory, mode 0700, with every file created exclusively at 0600 whatever the umask. The command shall take no other destination, shall refuse where a name it needs exists or is a link, shall never change the mode or owner of anything that exists, and shall leave no partial directory when it fails. | O, G3; security review S4 | must | `crates/orch-cli/tests/db_backup_restore.rs::backup_is_consistent_under_concurrent_writes`, `::a_backup_is_private_under_a_permissive_umask`, `::an_existing_name_or_a_link_refuses_the_backup`, `::a_failed_backup_leaves_nothing`; both |
| R76 | `orch db restore` shall run with the daemon stopped, verify the backup against its manifest before changing anything, keep the replaced state aside, and leave the database, the blobs and the repositories as they were at the backup. The copy that R124 makes before a migration is the one exception: its manifest says that it holds the database alone, and restoring it replaces the database and leaves blobs and repositories as they are. | O, G3 | must | `db_backup_restore::backup_then_restore_round_trips_database_blobs_and_repos`, `::a_damaged_backup_is_refused_before_any_change`; both |
| R77 | A restore shall start a new generation and a new restore lineage. The lineage is a random 128-bit id made when a database is created, replaced by every restore, kept by every other start, stored in `daemon_config` and held by every lease beside its epoch. No pair of lineage and epoch shall be issued twice, which is gate line 9 as the owner amended it (AP11). An epoch's number alone may repeat under another lineage, and nothing shall compare the epochs of two lineages. A task's next epoch shall come from `task_epochs` alone and never from a clock. | O, G9 as amended; AP11; security review S6 | must | `crates/orch-cli/tests/restore_fencing.rs::no_lineage_and_epoch_pair_is_reused_after_restore`, `::a_restore_onto_an_empty_directory_starts_a_new_lineage`, `::a_clock_set_back_changes_no_epoch_and_no_lineage`, `::lineage_survives_a_normal_restart`; both |
| R78 | A restore shall list every identity and token that the backup holds enabled, mark those the replaced state had disabled or revoked, and proceed only on a typed confirmation or `--yes`; without a terminal and without `--yes` it shall exit 2. | O, G9 | must | `restore_fencing::restore_lists_reenabled_identities_and_tokens`, `::restore_without_confirmation_changes_nothing`; both |
| R79 | `orch db prune` shall run the event pruning now, with `--dry-run` and `--older-than`, and `orch db vacuum` shall rebuild the database with the daemon stopped. | O | must | `db_commands::prune_dry_run_deletes_nothing`, `crates/orch-cli/tests/db_offline.rs::vacuum_runs_offline_and_keeps_every_row`; both |
| R80 | `orch db export` shall write one file holding one project's rows and blob bytes, in the export format of "Import bounds"; `orch import --from <file>` into a data directory that holds no database shall reproduce every exported row and every blob hash. The product's own export shall pass every check and limit that R113 and R114 put on an export. | O, B8, G4; security review S1 | must | `crates/orch-cli/tests/db_export_import.rs::export_then_import_reproduces_every_row_and_hash`, `::a_truncated_export_is_refused`, `crates/orch-cli/tests/export_hostile.rs::the_products_own_export_passes_every_limit` (with blobs of 0 bytes, of 1,048,576 bytes and of one byte more); both |
| R81 | Only the daemon shall write the attention queue; at most one open item shall exist per project, kind and subject; the owner shall be able to list, show and acknowledge items, and `ack` shall be the only action on one. What an item says and where it came from are fixed by R131. | P; security review S5 | must | `crates/orch-engine/tests/attention.rs::one_open_item_per_subject`, `crates/orch-cli/tests/attention.rs::owner_lists_and_acknowledges`; both |
| R82 | While free space is under the floor, a blocking `disk_floor` item shall be open; it shall resolve by itself when free space is back above the floor plus 1 GiB. | P, B11 | must | `attention::disk_floor_opens_and_resolves` (disk probe seam); both |
| R83 | While dispatch is off and a task that could be dispatched has waited 10 minutes, a warning `dispatch_off_with_work` item shall be open; it shall resolve when dispatch turns on or no such task remains. | P | must | `attention::dispatch_off_with_work_opens_and_resolves` (fake clock); both |
| R84 | Resolved attention items shall be deleted after `attention.resolved_retention_days` (90); `orch doctor` shall list open items and exit 1 while a blocking item is open. (O6) | P, O6 | must | `maintenance::resolved_attention_expires`, `crates/orch-cli/tests/attention.rs::doctor_fails_on_a_blocking_item`; both |
| R85 | While free space on the filesystem of the data directory or of the project's workspaces is under the larger of 5 GiB and 5% of that filesystem, the daemon shall start no run and shall refuse operations that add work with `quota_exceeded`, and shall keep finishing, cancelling, reading and pruning. | B11 | must | `crates/orch-engine/tests/disk_floor.rs::no_run_starts_under_the_floor`, `::new_work_is_refused_with_quota_exceeded`, `::running_work_still_finishes`; both |
| R86 | Every run shall get its usage recorded when it ends, whatever its outcome: the five token counts, each empty when the agent CLI reported none, never zero in its place. | Q | must | `crates/orch-engine/tests/usage.rs::usage_is_recorded_for_every_outcome`, `::unreported_tokens_are_null_not_zero`; both |
| R87 | Every run shall record the model that ran and the agent CLI's version, each empty when it cannot be learnt, and a failure to learn either shall not fail the run. | Q | must | `usage::model_and_adapter_version_are_recorded`, `::a_failed_version_probe_does_not_fail_the_run`; both |
| R88 | The manual, the RUNBOOK, ARCHITECTURE, the README upgrade notes, the CHANGELOG and a storage reference under `docs/` shall describe 1.1 as built, and the manual shall say plainly that `preapproval` is implicit trust of the agents. | R, B14 | must | Fact check of each document against the code at the candidate commit; `cargo test -p xtask` (ARCHITECTURE checks); Linux. The README upgrade notes, the CHANGELOG and the release PR shall say that the release was qualified on Linux only and that macOS is untested until after 2.0 (record K); a reviewer checks that they do |
| R89 | Remote peers shall keep working beside a 1.1 daemon with the peer protocol unchanged, at the cost of the source changes listed in "orch-peer". (AP1) | B15 | must | `cargo test -p orch-peer`; a 1.0 peer and a 1.1 daemon exchange a task and its result in both directions (recorded check); Linux, and a second Linux host |
| R90 | No fixture, test, PR or record shall contain content from a real project; fixtures shall be generated from a seed or written by hand. (O10) | O10, DL | must | `cargo xtask fixture-gen --check` reproduces every generated fixture byte for byte; reviewer check of hand-written ones; both |
| R91 | No test shall reach a real project's daemon, the real `~/.orchestry` or a data directory outside its own temporary root. | DL | must | `./xtask/gate.sh pr` (`home-hygiene`, `real-home`); Linux |
| R92 | Every PR of the stack shall leave `main` green and working, with the file backend working until the PR that removes it. | DL, B1 | must | CI at each PR's final head; `cargo test --workspace` at each merge; Linux CI |
| R93 | Only an owner session shall set `preapproval`; an agent's own `approval_policy` of `auto` shall have effect only while the project's `preapproval` is on. R133 names every path that turns it on. | B14 | must | `crates/orch-engine/tests/preapproval.rs::auto_needs_the_project_policy`, `ops_authorization::only_the_owner_sets_preapproval`; both |
| R94 | — | X | won't | Evidence, quarantine, uploads, text search, HTTP, TLS, tokens, enrolment, certificates, workers, the hub, landing, proposals, many projects per daemon, removing orch-peer, Postgres, an indexed scheduler, object storage, token budgets, outbound notification and Windows are out of scope (X). |
| R95 | — | DL | won't | No tag and no version bump: cutting 1.1.0 is the owner's separate decision. |
| R96 | An agent process shall not run its agent unless the run's pid and start time were committed first: the daemon shall create the process held, commit, then release it, and a held process whose daemon is gone shall exit without running the agent. | review finding (a stop between starting a process and recording it left an untracked agent on a workspace that recovery would hand to a second run) | must | `crates/orch-engine/tests/transactions.rs::a_stop_between_spawn_and_commit_runs_no_agent`, `crates/orch-infra` process test `held_process_exits_when_its_parent_is_gone`; both |
| R97 | A text value over 64 KiB, written through the API or imported, shall be stored in the blob store, with the row holding its hash, and shall be returned whole on read. | B7; review finding | must | `text_over_64_kib_round_trips_through_the_blob_store` in `storage_conformance` checks whole values on entity references; `storage::v2::tests::text_over_64_kib_round_trips_through_the_blob_store` in the `orch-infra` library pins file row hashes and disk bytes, with `inline_threshold_counts_utf8_bytes_and_preserves_empty_values`. Retry rows were not covered on `main` at `07b5f967`, which refused text over 65,536 bytes; this change covers them by the tests that "T8 consumer-contract amendment (issue 109)" names (65,536 and 65,537 bytes in `retry_whole_text_round_trips_at_the_utf8_boundary_without_a_run`, registered as `memory_retries_v2`, `blob_memory_retries_v2` and `file_retries_v2`; and 67,108,864 and 67,108,865 bytes, a character split across a chunk boundary, a failed publication, a failure on a later chunk, a missing and a corrupt blob and chunk, a reordered chunk, repeated chunks, a rollback that leaves published bytes unreferenced and a one-object row of the documented form in `storage::v2::tests::retry_*` of the `orch-infra` library, each run with `-- <name> --exact` and required to execute one test; a retry with no run is in the first, a restore of 67,108,865 bytes with no run is in `retry_chunked_text_survives_restore_without_a_run`, and a reordered chunk of the disk-backed fake is in the private unit test `store::retries::fake::tests::reordered_retry_chunks_fail_every_memory_retry_read`). SQLite backing assertions: T10; importer backing assertions: T21 (pending, no executable import filter yet); Linux |
| R98 | Reads shall be served by a pool of `storage.read_connections` connections (default 4); a read that finds none free shall wait up to the admission deadline and then fail with `overloaded`, never open another connection. | B6; review finding | should | `sqlite_writer::reads_wait_for_a_free_connection_then_fail_overloaded`; both |
| R99 | `orch import` shall account for every entry of the source `.orchestry/` and of each directory that "Import rules" has it list: each is imported, or left in place as "Import rules" lists, or named by the dry run and by the import's report as not imported; a directory it does not enter is one entry. Project prompt template files shall become `templates` rows, byte for byte. | N, S2, B2 | must | `import::every_source_path_is_accounted_for` (a corpus with an unknown file and an unknown directory), `::templates_are_imported_byte_for_byte`; both |
| R100 | `orch db restore` shall refuse, changing nothing, while a process that the replaced state or the backup records as a run's is alive (pid and start time), naming each run and pid and which of the two records it. With `--stop-runs` it shall first stop the process groups that the replaced state records, which this daemon wrote in this data directory. It shall never signal a process because a backup records it: such a process keeps the restore refused until it has ended or the owner names its run with `--assume-stopped <run id>`. No waiting period shall stand in for either: a run's process in 1.1 keeps no deadline of its own (R42), so the passing of time never shows that an earlier holder has stopped. After a restore no lease of the backup shall be active: each is ended and its run closed as R41 closes a run whose process is gone, and the spool of the replaced state (R129) is set aside with it. | G9; the lease prototype and its review (a restore that frees every task at once lets a holder from the replaced state and a new one work on one task); the security re-review's condition S2-C2 (a pid and a start time do not show who wrote a backup); the review of the prototype's corrections, N1 (its waiting-period alternative holds only where a holder stops itself, which arrives in 1.5); AP14 | must | `crates/orch-cli/tests/restore_fencing.rs::restore_refuses_while_a_recorded_process_is_alive`, `::stop_runs_stops_only_what_the_replaced_state_records`, `::a_process_only_the_backup_records_is_never_signalled` (a fake process table records every call), `::assume_stopped_lets_the_restore_proceed_for_the_named_run`, `::restored_leases_are_ended`; both |
| R101 | No path component below the data directory, below the data home's `projects/`, in a backup or in an export shall be taken from an id, a key, a name or a path that a request, an import source, a backup's manifest or a database row supplied: each shall be a fixed name, 64 lower-case hex digits of a SHA-256 the daemon computed, the leading two hex digits of such a hash as a shard directory, or a name the daemon generated, as "Filesystem names" lists them. A run's event file shall be named by the whole hash defined there, and a hash read from a row shall select a file only after it parses as 64 lower-case hex digits. A name found in a hashed directory that has not its form shall be reported by `orch db verify` and never opened or deleted. | security review S2 (the layout `blobs/events/ab/<run id>` left the data directory for the run id `../../../../outside-canary`) | must | In 1.1, R129's tests of the finish file, the one file that 1.1 names from a run id: `crates/orch-infra/tests/fs_names.rs::the_finish_file_name_is_the_whole_hash_for_any_run_id`, `crates/orch-engine/tests/leases.rs::a_finish_is_spooled_and_removed_under_its_hash_name_for_any_run_id` and `::recovery_reaches_a_spooled_finish_under_its_hash_name_for_any_run_id` (the finish-file vectors of "Filesystem names": traversal-shaped and absolute run ids, one holding a NUL, one holding a separator, `..`, and the two ids split at another point; an outside canary is unchanged through the write, the recovery and the removal). `crates/orch-infra/tests/event_files.rs::the_file_name_is_the_whole_hash_for_any_run_id` (the same inputs on the event file's name and path) and `::read_tail_and_prune_reach_only_the_hashed_file` are due only with the event fallback, which 1.1 does not build (R63). Also `crates/orch-infra/tests/blob_store.rs::a_hash_that_is_not_64_hex_digits_is_refused_and_touches_nothing`, `maintenance::a_stray_name_in_the_blob_tree_is_reported_and_not_deleted`; `git grep -nE 'std::fs::\|tokio::fs::\|PathBuf' crates/orch-infra/src/blob crates/orch-infra/src/import crates/orch-infra/src/sqlite/backup*` prints nothing, so that only the module of R102 builds a name below a root; both |
| R102 | Every create, open, rename and delete of a file below the data directory, below the data home's `projects/`, in a backup and in an import source, and every write of `.orch-project` and of the authored files in the project directory, shall go through one module that reaches its file by directory descriptors opened from its root, one name at a time and following no link. The module shall refuse a name that is empty, `.`, `..` or holds a separator or a NUL; shall create a file exclusively with mode 0600 below the data directory (0644 for `.orch-project`) and a directory with 0700; shall judge an entry by the descriptor it has just opened, never by an earlier look at the name; and shall refuse, with a typed error and no change inside or outside the root, a symbolic link and an entry that is neither a regular file nor, where the layout expects one, a directory. A file with more than one hard link shall be refused in an import source, in a backup being restored, and wherever a file is opened to be appended to or changed in place. The files SQLite opens itself and the socket are the exceptions that "Filesystem names" states. | security review S2 | must | `crates/orch-infra/tests/confined_dir.rs::a_symlink_at_any_level_is_refused_and_not_followed`, `::a_name_that_is_not_one_component_is_refused`, `::a_hard_link_is_refused_for_append_and_in_untrusted_trees`, `::a_fifo_and_a_socket_are_refused_without_blocking` (the test has a time limit), `::an_entry_swapped_after_the_listing_is_judged_by_its_descriptor`, `::rename_replaces_a_planted_link_and_never_follows_it`, `::rename_and_delete_stay_inside_the_root` (outside canary), `::new_files_are_exclusive_and_0600` (permissive umask), `blob_store::a_planted_link_in_the_blob_tree_is_refused`, `event_files::a_planted_link_is_refused_on_append_read_and_prune` (needs an event file: due only with the event fallback, which 1.1 does not build, R63); both |
| R103 | `orch import` shall find its source files only by listing the directories that "Import rules" names, from a descriptor of the source `.orchestry/`, and shall open an entry only by its listed name, as R102 says. It shall open no path built from a value inside a document (an index entry, an attachment name, a workspace path, a configuration value) and shall look such a value up among the listed names instead. A listed entry shall be a regular file with one hard link where "Import rules" expects a file and a directory where it expects a directory; any other entry there (a symbolic link, a FIFO, a socket, a device, a file with more than one hard link) shall stop the import before any of its bytes is read, named by its escaped path relative to the project, and the dry run shall list each such entry. | security review S2 | must | `crates/orch-cli/tests/import.rs::source_links_and_special_files_are_refused_unopened` (a symbolic link to a file as a store file and as an attachment, a symbolic link to a directory as a store directory, a hard-linked store file and attachment, a FIFO and a socket as store files; the canary's bytes reach neither the database nor the blob store, and the test has a time limit), `::names_inside_documents_are_looked_up_and_never_opened` (index entries, attachment names and workspace paths that are traversal-shaped or absolute and name the outside canary), `::unknown_directories_are_named_and_not_entered`; both |
| R104 | An imported entity's id shall be the stem of the file it was read from. The import shall stop, naming the file, unless the document's own id (for a context entry, its key), where the document carries one, equals that stem byte for byte and the stem holds only `A-Z a-z 0-9 . _ -`, the names the 1.0 store could write; a task whose id is `.` or `..` shall stop it likewise. This row judges one id at a time and decides nothing about a filesystem: the task ids of a source shall pass R117 together, which refuses two that differ only in the case of ASCII letters and rewrites neither. | security review S2; its re-review, finding S2-R1 | must | `import::an_id_that_is_not_its_file_stem_is_refused_by_path` (document ids `../../../../outside-canary` and `/outside-canary`, one holding a NUL, one holding `/`, a stem holding a space, and a task `..`; nothing is committed for the file and the outside canary is unchanged); R117's tests for a pair of ids; both |
| R105 | `orch import`, from a 1.0 directory or from an export file, shall write no lease and no intent, shall signal no process and look one up only for R119's check of whether a recorded pid's process or process group exists, and shall create, change or remove nothing in the project directory except `.orch-project` and, where it is missing, the empty directory `.orchestry/` that the handshake needs. A pid, a workspace path and a project directory read from a source shall be stored as text or left out, and nothing shall later signal a process or create, open or remove a directory because of them. In-flight 1.0 state that R119 lets pass shall be closed in the database as if its process were gone, and a run that an export holds unfinished shall be ended at the first start, without reading its pid, by R41's rule for an unfinished run without a lease. (AP14) | security review S2; red team I1; F8 and `docs/internal/orchestration.md` "Startup recovery" (1.0's recovery kills the process a run document names and keeps a running entry whose pid is alive) | must | `import::source_pids_and_workspace_paths_are_never_acted_on` (a running entry that the owner named with `--assume-stopped` and a run document name the pid of a live canary process and a canary directory outside the project; after the import, the first start and a cancel of the task, the process lives and the directory is unchanged), `::import_writes_no_lease_and_no_intent`, `crates/orch-cli/tests/db_export_import.rs::an_unfinished_run_is_ended_at_the_first_start_without_a_signal`; both |
| R106 | The daemon shall address a task's workspace only as `workspaces/<task id>` below the project's `.orchestry/`, never by a stored path, after checking that the id is one name of `A-Z a-z 0-9 . _ -` other than `.` and `..` and that neither `workspaces` nor that entry is a symbolic link. A checked id is one name and no more: the daemon shall reuse, enter, create or remove that directory only when R118 has shown it to be this task's and no other name's. It shall merge or delete the branch of a task's local run only when the name stored for that run (`runs.branch`) is `orchestry/<task id>/` followed either by 1 to 40 characters of `a-z 0-9 -` or by the task id again, which is 1.0's name when the title gives no slug, and when R118 finds a ref of exactly that name. Otherwise it shall fail the step with `workspace` and change nothing in the project directory or its repository. (AP14) | security review S2; its re-review, finding S2-R1 (an alphabet shows that an id is one name, not that the directory is one task's); 1.0 merges and deletes the branch a task document names (`orchestrator/collect.rs`, `workspace/manager.rs`) | must | `crates/orch-infra` workspace tests `a_task_id_that_is_not_one_name_gets_no_workspace`, `a_link_at_the_workspace_name_is_refused` (outside canary), `crates/orch-engine/tests/intents.rs::workspace_remove_takes_a_task_id_never_a_path`, `crates/orch-engine/tests/transactions.rs::a_stored_branch_of_another_shape_is_neither_merged_nor_deleted` (a run whose `branch` is `main`, and one whose branch names another task); R118's tests for a directory and a branch under another spelling; both |
| R107 | `.orch-project` shall be read without following a link and accepted only as a regular file of at most 64 bytes holding one project id of 1 to 63 bytes of `A-Z a-z 0-9 _ -` and a newline; a pointer that exists and is anything else shall get exit 2, naming the file, and only an absent one exit 3 (R34). It shall be written only by an exclusive create that follows no link; an entry already at that name shall be left as it is and shall stop the command unless it is a valid pointer to this project. The record of a project's data directory shall be the file `<data home>/projects/<64 hex digits of the SHA-256 of the project id>`, read without following a link and accepted only as a regular file of at most 4,096 bytes holding one absolute path and a newline, else exit 2 naming the record; the directory it names shall pass R29's checks before anything in it is opened. A project id read from an export shall pass the pointer's check before it is stored. | security review S2 (the pointer file comes with a checkout and an export file from elsewhere, and the project id named the record file) | must | `crates/orch-cli/tests/project_pointer.rs::a_pointer_that_is_not_one_project_id_is_refused_with_exit_2` (a traversal-shaped id, an absolute path, a NUL, a separator, two lines, 65 bytes, a symbolic link), `::a_link_at_the_pointer_name_is_never_written_through` (`orch init` and `orch import` with `.orch-project`, `.orchestry/.gitignore` and `workspace-exclude` planted as links to an outside canary), `crates/orch-daemon/tests/daemon_data_dir.rs::the_project_record_is_named_by_the_hash_of_the_id`, `::a_record_that_is_not_one_absolute_path_is_refused` (a link, a relative path, two lines, 4,097 bytes, a directory open to others), `db_export_import::a_project_id_outside_the_alphabet_is_refused`; both |
| R108 | `orch db restore` and `orch import --from` shall reach a backup's or an export's contents only by the fixed layout and by hashes they compute. A restore shall list the backup as R102 says and refuse it, before any change, unless the listed names and the manifest's names are the same set and each is a name of the backup layout in "Data directory". An import shall store a blob of an export under the hash of its bytes and refuse a blob whose stated hash differs, and shall refuse a task id that fails R104's check before it stores the row. Both shall refuse, before a database is published or anything is replaced, what R117 refuses: two task ids of one fold key, and a directory in the project's `workspaces/` that the tasks they bring may not own. No name read from a manifest or an export shall be opened as a path. | security review S2; its re-review, finding S2-R1 | must | `crates/orch-cli/tests/db_backup_restore.rs::a_manifest_name_outside_the_layout_is_refused_before_any_change` (`../outside-canary`, an absolute name, a name holding a NUL, one file more and one file fewer than the manifest), `::links_and_special_files_in_a_backup_are_refused_before_any_change`, `::event_files_round_trip_under_their_hash_names` (if the fallback ships), `db_export_import::names_in_an_export_are_stored_and_never_opened` (run ids, attachment names and workspace paths that name an outside canary; a blob whose stated hash is `../x`), `::a_task_id_that_is_not_one_name_is_refused` (`..`, `a/b`, one holding a NUL); R117's tests for an export and for a backup; both |
| R109 | Before `orch import` builds any value from a 1.0 source document it shall have checked every budget that "Import bounds" gives for the document's kind, in one pass over the document's parse events that expands no alias, builds no tree and recurses on nothing the document controls: the size in bytes, that a YAML stream holds exactly one document (a stream of none is refused as a stream of two is, with the limit 1 and the count it found), the nesting depth and the node count of the document as expanded, the number of aliases, the bytes of the document as expanded, that no alias stands inside its own anchor, and that no node carries an explicit tag. It shall read at most the byte bound and one byte more of any file, and the check and the deserialisation shall work on the same bytes in memory. A document over a budget shall be refused under that rule's name and never built, in whole or in part. | security review S1 (a byte bound alone lets a 40,001-byte document nest 20,000 deep and a 412-byte document stand for 100,000,000 scalars); F12 | must | `crates/orch-infra/tests/source_budget.rs::each_yaml_budget_passes_at_its_limit_and_refuses_one_over`, `::each_json_budget_passes_at_its_limit_and_refuses_one_over`, `::the_depth_bomb_is_refused_without_recursion` (the review's document, as YAML and as JSON, on a thread with a 256 KiB stack), `::the_alias_bomb_is_refused_without_expansion` (the review's 412-byte document, and 1.0's 84,661-byte document of 60 aliases that expands to 2^30 nodes), `::expanded_depth_counts_through_aliases`, `::an_alias_inside_its_own_anchor_is_refused`, `::an_explicit_tag_of_any_kind_is_refused`, `::a_second_document_is_refused`, `::a_stream_of_no_document_is_refused` (zero bytes, spaces and newlines only, comments only; `documents`, limit 1, observed 0, and the text `expected one document, found 0`), `::a_count_that_would_wrap_is_over_budget`, `::an_endless_reader_is_read_no_further_than_the_bound`; both |
| R110 | A YAML document that holds aliases or merge keys and is inside the budgets shall be imported with the value 1.0 read from it: every alias expanded and every merge key resolved as 1.0's reader resolves them (F12). One budget of expanded nodes and one of expanded bytes shall count every expansion in the document, whether the alias stands as a value or under a merge key. Every document that 1.0 wrote shall pass every budget. Where a budget is stricter than 1.0's reader (the four places "Import bounds" lists: an explicit tag, a depth of 65 to 128, expanded bytes over the budget, and a `config.yml` whose stream holds no document), the refusal shall say that 1.0 read the document. The fourth place was missing from this row until 2026-11-22, when it said three; that the import refuses there at all is drafted from the unanswered default of T21-C1. (AP15) | security review S1 (do not disable aliases blindly: 1.0 resolves merge keys); B3; F12 | must | `crates/orch-cli/tests/import.rs::a_document_with_merge_keys_imports_as_1_0_read_it` (the merge-key documents of 1.0's own reader tests: one anchor merged twice, a merge of a list of anchors, a key set again after a merge), `::documents_written_by_1_0_pass_every_budget` (the generated corpus of the live project's shape, R90), `source_budget::merge_keys_draw_on_the_same_budget_as_aliases` (a merge-key bomb, and a document whose aliases and merge keys pass 200,000 nodes together and not apart), `::a_refusal_stricter_than_1_0_says_so` (each of the four places); both |
| R111 | `orch import` shall read a run's event file as a stream that holds at most one line of 65,536 bytes and one byte more. A line that is longer, that nests deeper than 64, or that does not parse shall not be stored; the rest of an over-long line shall be skipped to the next newline without being kept; and lines not stored that follow one another shall be recorded as one gap, so that a file adds at most one gap more than it adds events. A source that lists more than 1,000,000 entries shall be refused before any document is read. Free space shall be checked again at every batch, and an import under the floor shall stop as R112 says for a stop. | security review S1; F4 | must | `crates/orch-cli/tests/import_hostile.rs::a_line_without_a_newline_is_not_kept` (one line of 256 MiB), `::unusable_event_lines_become_one_gap` (over-long, over-deep, not UTF-8 and not JSON lines, in a row and between stored events), `::a_listing_over_the_entry_limit_is_refused_before_any_document_is_read` (the limit injected), `import::free_space_is_checked_at_every_batch` (disk probe seam); both |
| R112 | `orch import` from a 1.0 directory shall first do everything its dry run does. Where that finds a refusal it shall exit 2 having created and changed nothing below the data home and nothing in the project directory; on a `--resume`, nothing beyond what the stopped import left. A stop after its first write (a source that changed since it was checked, an input or output error, the disk floor, a stopped process) shall exit 1 and leave an unfinished import: the transaction in progress rolled back, no document stored in part, no pointer file, a daemon that refuses to start on it and `--resume` able to continue (R67). The report shall name, for each of at most 1,000 refusals and with a count of the rest, the escaped path, the rule, the limit and what was observed. The source shall be unchanged in every case (R64). | security review S1 | must | `import_hostile::every_hostile_document_is_refused_before_any_write` (each entry of the hostile corpus as a task, an agent, `config.yml`, a run, `state.json` and an index; exit 2, the rule and the path in the `--json` report, no file below the data home, a byte-identical source), `::the_report_of_refusals_is_bounded` (2,000 refused files), `::a_document_changed_after_the_check_stops_the_import_resumably` (a file replaced by a hostile one between the check and the load; exit 1, nothing of that document stored, and `--resume` completes once the file is put back); both |
| R113 | `orch import --from <file>` shall treat an export file as hostile. Before any SQL runs on it the importer shall have read its first 100 bytes itself and refused a file whose header is not that of an export ("Import bounds"). It shall open the file on a connection of its own, read-only and immutable, never attached to another connection, with defensive mode on, the schema untrusted, triggers and views off, extension loading unavailable, memory mapping off, cell-size checking on, the run-time limits and the heap limit of "Import bounds", an authorizer that allows only the statements listed there, and a progress handler that stops a statement when the import is cancelled or past its step budget. Its first statement on the file's content shall be `PRAGMA quick_check(1)`, and any answer but `ok` shall refuse the file. No statement shall be built from text read from the file, and no file beside it shall be read or created. | security review S1 (an export is an arbitrary SQLite file; its own manifest of hashes shows that it agrees with itself, not that it can be trusted); F13 | must | `crates/orch-cli/tests/export_hostile.rs::a_file_that_is_not_an_export_is_refused_by_its_header`, `::planted_companion_files_are_neither_read_nor_created`, `::a_corrupt_file_is_refused_by_the_integrity_check`, `::the_connection_is_configured_before_the_first_statement` (each setting and limit read back), `::no_function_trigger_view_or_extension_of_the_file_runs` (an outside canary is unchanged), `::a_statement_past_its_step_budget_is_stopped` (the budget injected); `git grep -n 'ATTACH' crates/orch-infra/src/import` prints nothing; both |
| R114 | An export shall be accepted only when its `sqlite_schema` is, row for row, the set this binary writes for an export of its own schema version: the project tables without `leases`, `intents` and `request_keys`, the manifest and the blob-chunk table, with the automatic indexes of their keys and nothing else. The importer shall create the destination by its own migrations and never from the file's schema, and shall copy rows only by the fixed statements of "Import bounds" and by inserts with bound values, so that every type, check, key and reference is enforced by the destination. It shall refuse a file with a row count other than its manifest states, a row of another project, a value over the row bounds, text that is not UTF-8, a blob chunk outside the chunk rules, blob bytes that do not hash to their name, a blob no row names and a row naming a blob the file lacks. It shall take over no lease, intent, request key, identity, token or daemon-wide setting from a file. | security review S1 | must | `export_hostile::a_schema_that_is_not_the_contract_is_refused` (one table, index, view, trigger or virtual table more; a generated column; a changed check; a missing table; `leases`, `intents` or `request_keys` present; 10,000 tables more), `::a_row_outside_the_contract_is_refused`, `::blob_chunks_outside_the_contract_are_refused`, `::no_runtime_or_daemon_wide_row_is_taken_over`; both |
| R115 | An import from an export file shall build its database under a scratch name and give it the name `orch.db`, and only then write the project's record and `.orch-project`, after every check of R113 and R114 has passed for every row and every blob. A refusal (exit 2) or a stop (exit 1) at any earlier point shall leave a data directory that holds no database, no record and no pointer; the scratch database shall be removed by the attempt, or by the next import where the attempt was killed; and a blob file already written shall be an unreferenced file named by the hash of its bytes. The data directory shall not be created before the file has passed its header, its integrity check, its schema and its manifest. | security review S1 (no partial publication) | must | `export_hostile::every_hostile_export_is_refused_before_anything_is_published` (each entry of the hostile export corpus; exit 2, the rule in the `--json` report, no `orch.db`, no record, no pointer, a byte-identical export), `::a_refusal_at_the_last_row_leaves_no_database`, `::a_killed_import_leaves_no_database_and_the_next_one_succeeds` (a stop injected at every batch boundary and before the rename), `::an_export_changed_during_the_import_is_refused`; both |
| R116 | For every entry of the hostile corpora of "Import bounds", an import and a dry run, each in a process of its own, shall end with the stated outcome within 30 s and with at most 512 MiB resident, whatever the entry says about its own sizes, counts, depth or aliases. The work an import does on a source shall be at most linear in the source's size in bytes. | security review S1 (bounded time and memory, shown by tests and not by a deadline) | must | `import_hostile::every_hostile_document_is_refused_before_any_write` and `export_hostile::every_hostile_export_is_refused_before_anything_is_published` measure both for each entry (the child's peak resident memory from its resource usage); `import::memory_stays_bounded_on_a_large_project`, `crates/orch-cli/tests/db_export_import.rs::memory_stays_bounded_on_a_large_export`; both |
| R117 | No two tasks of a project shall have ids with one fold key, which is the id with `A` to `Z` read as `a` to `z` ("Filesystem names"). The rule shall be the same on every platform, whatever the filesystem tells apart, and the key shall only ever be compared: no id shall be lower-cased, rewritten or renamed to pass, and every read of a task by its id shall stay exact to the byte. The schema shall enforce it by a unique index on `tasks` over the project and the id under SQLite's `NOCASE` collation, and the file reference by the same comparison. `task.create` shall draw another id, at most 8 times, when the one it drew is refused, and then fail and change nothing. `orch import`, from a 1.0 directory and from an export file, and `orch db restore` shall refuse, before they publish a database or replace anything, a source in which two task ids have one fold key (rule `id_alias`), and a project directory whose `.orchestry/workspaces/` lists a name that has the fold key of one of the source's task ids and that this task may not own (rule `workspace_alias`): for a 1.0 directory and for a backup, a name that is not that id byte for byte (for a backup this clause is provisional until the owner rules on the fifth point of AP14); for an export, which carries no workspace, any such name. They shall list `workspaces/` for its names alone and open or enter no entry of it; the refusal shall name both ids, or the id and the entry, and leave every directory as it was; a restore shall make the comparison itself and refuse a backup whose database lacks the index. The dry run shall report the same, and shall list a workspace that no task names as left in place; nothing shall adopt or remove such a workspace. (AP14) | security review S2, re-review finding S2-R1 (`tsk_Case` and `tsk_case` pass R104 and R106, are two rows with two leases, and are one workspace on a filesystem that folds case; F14) | must | The bare filters `two_task_ids_that_differ_only_in_case_are_refused` (`tsk_Case`, then `tsk_case` refused; `tsk_cas3`, and `tsk_case` in another project, accepted; a read of `tsk_case` finds nothing), `task_ascii_aliases_are_refused_without_rewriting_ids`, `task_lookups_stay_exact_to_the_byte` and `only_task_ids_fold_and_only_the_ascii_letters`, each selecting the 3 entity references (plan: "T8 consumer contracts (issue 109)"); `cargo test -p orch-infra --test sqlite_schema -- task_ids_are_unique_under_ascii_case_only_within_a_project`; the importer preflight unit test `import::preflight::tests::two_task_ids_that_differ_only_in_case_are_refused_before_any_write` in `orch-infra`'s library; `crates/orch-engine/tests/task_ids.rs::create_draws_again_when_an_id_folds_onto_an_existing_one` (a seeded generator), `crates/orch-cli/tests/import_hostile.rs::two_task_ids_that_differ_only_in_case_are_refused_before_any_write` (exit 2, `id_alias`, both paths in the `--json` report, nothing below the data home, a byte-identical source in which both ids stand as written), `::a_workspace_that_folds_onto_a_task_id_is_refused_and_left_unchanged` (task `tsk_case`, a directory `workspaces/tsk_Case` of mode 0000 holding a canary: exit 2, `workspace_alias`, the directory not opened, its bytes and metadata unchanged), `crates/orch-cli/tests/import.rs::a_workspace_at_its_tasks_exact_name_is_kept_and_one_without_a_task_is_reported`, `crates/orch-cli/tests/export_hostile.rs::two_task_ids_that_differ_only_in_case_are_refused_before_publication` (no `orch.db`, no record, no pointer), `::a_directory_already_at_a_tasks_name_is_refused_and_left_unchanged` (under the exact name and under another spelling, each with a canary), `crates/orch-cli/tests/db_backup_restore.rs::a_backup_with_two_task_ids_of_one_fold_key_is_refused_before_any_change` (also a backup whose database lacks the index), `::a_workspace_that_folds_onto_a_restored_task_id_is_refused_before_any_change`; both, and none depends on what the test's filesystem folds |
| R118 | Before the daemon reuses or enters a task's workspace, merges from it or removes it, it shall have shown the directory to be that task's: it opens `workspaces/<task id>` by that one name without following a link (R102, R106), and then requires that a listing of `workspaces/` holds an entry whose name is the task id byte for byte and which is the directory it opened (the same device and inode). An entry that opens under the id and is not listed under it is another name's directory: preparing shall fail with `workspace`, a `workspace.remove` shall end having removed nothing, and nothing shall be created, changed or removed in that directory, or in the repository on its account. A workspace shall be created only by an exclusive create of that one name, which fails when anything answers to it, followed by the same check; where the check then fails, the daemon shall remove the empty directory it has just made and fail the step. A branch shall be merged or deleted only when the repository's own listing of its refs holds the name R106 allows, byte for byte, and created only when no listed ref has that name's fold key under another spelling. Nothing shall adopt, enter or delete a directory or a branch that these checks do not show to be the task's, whatever the database holds. | security review S2, re-review finding S2-R1 (preparing reuses a directory that answers to the name, and cancelling removes it, so a second id read the first task's files and could delete them; one lease for each task does not serialise two ids on one directory; F14) | must | `crates/orch-infra` workspace tests `a_directory_under_another_spelling_is_not_reused` (a directory `tsk_Case` holding a canary, then preparing `tsk_case`: where the filesystem folds case, `workspace` and nothing reused or created; where it does not, a directory of its own; the canary unchanged in both), `removing_under_another_spelling_removes_nothing`, `a_new_workspace_is_created_exclusively`, `a_filesystem_that_does_not_keep_the_name_gets_no_workspace` (the listing injected), `a_branch_under_another_spelling_is_neither_merged_nor_deleted` (a branch `orchestry/tsk_Case/x` that no run recorded, loose and packed; its commit unchanged), `a_branch_is_not_created_beside_its_other_spelling`; `crates/orch-engine/tests/intents.rs::workspace_remove_leaves_another_names_directory`; `crates/orch-engine/tests/transactions.rs::cancel_failure_and_cleanup_leave_a_foreign_workspace_and_branch_unchanged` (for a task whose id folds onto the foreign names; canaries in the directory and on the branch); Linux. Three paths, none inferred from another. (1) The folding cases run against an injected folding listing on Linux. (2) The exact-name checks (the directory opened by that one name and listed under it byte for byte, the same device and inode) and every canary assertion run natively on the Linux filesystem the test uses, whether or not it folds case. (3) The real folding branch runs only where the test's temporary directory folds case; no Linux filesystem is required to, and none is provisioned for it (record K). Each test prints which of the three it ran, and one that did not run says so and why: the filesystem keeps case, or the test is not implemented yet. A path that did not run is not a pass and is not `env-limited`. None of the tests above is in the tree yet (they are due in T12 and T13); until each exists and has run, its row is pending implementation |
| R119 | `orch import` from a 1.0 directory shall refuse, changing nothing, while an entry of the running set of `state.json` may still be at work: a local entry whose recorded pid names a process that exists or a process group that still has a member, and every remote entry. 1.0 starts each agent as the leader of its own process group, and a child of the agent can outlive the leader in that group (F17), so a pid whose process is gone does not show that the run's work has ended. 1.0's startup recovery keeps a remote entry only while its peer is registered (F15); the import does not rely on the registry, because removing a peer from it stops nothing on the peer. The refusal shall name each entry by task, run, and pid or peer alias, and say whether the process or only its group was found. The check shall ask only those two questions, each by the existence test that delivers nothing (signal number 0): it shall deliver no signal and read nothing of a process, and the import shall have no option that stops one. A recorded pid of 0 or above 2,147,483,647 shall be passed to no system call and names nothing, and the group question shall not be asked for pid 1, where it would mean every process. `--assume-stopped <run id>`, given once for each run, shall be the owner's statement that the run has ended; the import shall then close that run as R69 closes one whose process is gone, and shall record the statement in `import_manifest` and in its report. Where the process or the group of a named local run still exists, the import shall show the pid and which of the two was found, and proceed only on a typed answer or `--yes` (exit 2 without a terminal and without `--yes`). A run id that names no such entry shall be refused with exit 2. The dry run shall list the same entries and say that the import would refuse. | Red team I1 (blocking) and its residual I1-R1 (the leader's pid alone misses a child that outlived it); N; AP14 | must | `import::refuses_while_a_recorded_process_exists`, `::refuses_while_only_the_recorded_process_group_has_a_member` (the leader gone, a child of its group alive), `::a_pid_of_0_or_1_or_over_the_range_is_never_passed_to_a_group_test`, `::refuses_every_remote_entry_whatever_the_peer_registry_says` (peer registered, removed, registry unreadable), `::assume_stopped_closes_only_the_named_run`, `::a_named_run_whose_pid_or_group_exists_needs_confirmation`, `::the_liveness_check_sends_no_signal` (a fake process table records every call and sees only existence tests), `::dry_run_lists_live_and_remote_entries`; both |
| R120 | A run that an import closed shall never be attached again: 1.1 shall start no sync for it, and an outcome that a peer reports for an execution that no lease of the database names shall change no task, no run and no branch, and shall be counted and logged. | Red team I1 | must | `import::a_closed_remote_run_is_not_reattached`, `crates/orch-engine/tests/peer_facade.rs::an_outcome_without_a_lease_changes_nothing`; both |
| R121 | A finished import from a 1.0 directory shall record in `import_manifest` the size and SHA-256 of `state.json` and of every `_index.json` it read. Every start of a 1.1 daemon, and `orch db status`, shall compare them with the files, read by listing as R103 says and writing nothing; a file that is gone is no difference. When one differs the start shall be refused, naming each changed file and the import's time, because the 1.0 state changed after the import and the two histories may have parted. `orch daemon start --accept-legacy-change` shall record the files as they are now and start with dispatch off (R44). | Red team I2; B2; AP16 | must | `crates/orch-daemon/tests/daemon_legacy_change.rs::start_is_refused_after_the_1_0_state_changed`, `::an_unchanged_or_removed_1_0_state_starts`, `::accepting_the_change_records_it_and_starts_with_dispatch_off`, `::the_check_writes_nothing_under_orchestry`, `db_commands::status_reports_a_changed_1_0_state`; both |
| R122 | Where an `_index.json` entry and the entity's own file disagree, the file shall win: the index gives membership and order only (R69), and the dry run and the report shall list the entity as `index_differs`. An `_index.json` that is missing, is not an array or does not parse shall be treated as an index that lists nothing: every entity file of that directory is imported in id order, and the directory is listed as `index_unreadable`. Those three cases are the whole of `index_unreadable`, and "does not parse" means a file that was read in full within its bounds and is not JSON. An `_index.json` that parses to an array shall be examined whole before its class is chosen. Where any entry is not an object or has no string identity (`id`; `key` under `context/`), the import shall stop as `index_entry` and say that 1.0 rebuilt such an index. Otherwise, where one identity stands in the array more than once, compared byte for byte, the import shall stop as `index_duplicate`, name the identity, and say that 1.0 used such an index as it stood only where every entry of the array was a complete entity of the directory's type, and otherwise rebuilt it from the entity files (F19). It shall stop alike in both cases and shall not say which of the two 1.0 did with this index: it judges an entry by its identity alone and cannot know. It shall never choose between the occurrences. The two stops are drafted from the unanswered defaults of T21-X1 and T21-X2. The importer shall judge an index entry by its identity alone and shall never run 1.0's index reader on the source (R64). A file-kind refusal (R103), a budget refusal (R109) and an input or output error on an `_index.json` shall stop the import and shall never be read as `index_unreadable`. | Red team I3; F16; [issue 146](https://github.com/yuxhuang/r-orch/issues/146); F19 | must | `import::the_file_wins_over_its_index_entry`, `::an_unreadable_index_is_rebuilt_in_id_order`, `::dry_run_report_lists_every_problem_class`, and the vectors X-V1 to X-V14 of "Vectors for `config.yml` and for an index", each with its test; both |
| R123 | For every problem class of R65 the table "Import problem classes" shall say whether the import stops or goes on, and the import shall do exactly that; a class that stops shall stop the check, before anything is written (R112). A document that the importer cannot read shall stop it. 1.1 shall have no option that skips a source file or sets one aside. (AP17) | Red team I4 | must | `import::every_problem_class_stops_or_goes_on_as_its_table_says` (one corpus case for each row, the rows added on 2026-11-22 included: `index_entry`, `index_duplicate`, an input or output error, and the `config.yml` rows), `::an_unreadable_document_stops_before_any_write`; both |
| R124 | Before it applies a pending migration to a database that holds a project, the daemon shall write `backups/pre-migration-v<from>-<UTC time>/` with a copy of the database made through the online backup API and a manifest holding its hash, shall check the copy with `integrity_check` and against the manifest, and only then migrate. It shall refuse the start, changing nothing, when free space is under the disk floor plus the database's size, or when the copy fails its check. It shall keep the two newest such copies and remove older ones after a start that migrated. The copy holds no blobs: `orch db restore` of it replaces the database alone and is a restore in every other respect (R77, R100), and `orch db verify --blobs` then names each blob deleted since. This is not a scheduled backup; 1.1 has none (AP9). | Red team I5; `docs/sqlite-library.md` (a backup before a migration); AP9 | must | `crates/orch-infra/tests/sqlite_migrations.rs::a_pending_migration_is_preceded_by_a_checked_copy`, `::the_start_is_refused_without_space_for_the_copy`, `::a_copy_that_fails_its_check_stops_before_the_migration`, `::only_the_two_newest_copies_are_kept`, `db_backup_restore::a_pre_migration_copy_restores_the_database_alone`; both |
| R125 | A stop after an import is recorded as finished and before `.orch-project` is written shall be repaired by running the same `orch import` command again: where the data directory holds a finished import of this project directory and the pointer is absent, the command shall write the pointer (R107), change nothing else and say so, where R68 would otherwise refuse a second project. The dry run shall report that state. | Red team I6 | must | `import::a_stop_before_the_pointer_is_repaired_by_running_again` (a 1.0 directory and an export file), `::the_dry_run_reports_a_finished_import_without_its_pointer`; both |
| R126 | The writer's connection shall run with automatic checkpointing off (`wal_autocheckpoint = 0`): no commit of the writer shall run a checkpoint. The `wal.checkpoint` job shall run passive checkpoints on a connection of its own, outside the writer task and outside the lanes, when 5 minutes have passed since its last completed checkpoint or when the log's backlog reaches 64 MiB, which it shall look at once a second at least. The backlog is a logical measure: the frames written to the write-ahead log and not yet copied into the database, times the page size, within one generation of the log (a log that starts again after a complete checkpoint counts from nothing). It shall be kept from SQLite's own counts, the writer's commit callback and each checkpoint's result; the result is exact and resets it, and where a change of generation cannot be told the larger backlog shall be assumed. It shall never be the length of the `-wal` file, which SQLite does not shorten after a checkpoint, and no checkpoint shall be run to learn it. `writer_commit_seconds` shall record the time of each commit apart from `writer_txn_seconds`; `wal_bytes` shall be the backlog and `wal_file_bytes` the file's length, shown apart, and no rule shall act on the file's length. Each checkpoint's start, duration, result and frame counts shall be recorded. Every connection that can write, the checkpoint connection included, shall set `fullfsync = ON` and `checkpoint_fullfsync = ON` and read both back; "Writer lanes" says what they mean on each platform, and they change nothing in `synchronous`. The sync settings are a proposal under AP13. The owner's ruling on that proposal is deferred with the measurement (record I of "Owner's answers"), and in 1.1 the settings are built as this requirement states them. | Red team W1 and its residual W1-R2 (a trigger on the file's length latches: the file keeps its size after every frame has been checkpointed); AP13 | must | `crates/orch-infra/tests/sqlite_settings.rs::the_writer_never_checkpoints_in_a_commit`, `::every_writing_connection_reads_back_its_sync_settings`, `sqlite_writer::checkpoints_run_on_their_own_connection`, `::the_backlog_is_zero_after_a_complete_checkpoint_while_the_file_keeps_its_length`, `::a_backlog_of_64_mib_starts_a_checkpoint`, `crates/orch-daemon/tests/ops_metrics.rs::commit_time_is_recorded_apart`, `::backlog_and_file_length_are_reported_apart`; both |
| R127 | While the log's backlog (R126's measure, not the file's length) is over 256 MiB, a read transaction of the pool that has been open for more than 30 s shall be interrupted with the typed error `snapshot_expired`, so that a checkpoint can copy the frames it holds back; a read under either limit shall be left alone, and so shall every read while the backlog is under the limit, however long the file. `orch db status` shall show the backlog, the file's length and the age of the oldest read. Both numbers are proposals that no measurement backs yet. | Red team W1, W1-R2 | should | `crates/orch-infra/tests/sqlite_pool.rs::a_long_read_is_interrupted_when_the_backlog_is_over_its_ceiling`, `::a_read_under_either_limit_is_left_alone`, `::a_long_file_with_no_backlog_interrupts_no_read`; both |
| R128 | The writer shall handle a transaction that fails for a reason outside its own logic by the classes of "Writer lanes". A retryable failure (`SQLITE_BUSY` or `SQLITE_LOCKED` past the busy timeout, `SQLITE_FULL`, a first `SQLITE_IOERR`) shall be rolled back and tried again after 50 ms, doubling to at most 2 s, and never dropped; no claim shall be granted while it lasts. A request answered `overloaded` at its deadline shall be withdrawn and never committed afterwards. `SQLITE_CORRUPT`, `SQLITE_NOTADB`, a second `SQLITE_IOERR` in a row, and one transaction retried for 60 s without committing shall fail the writer. A writer with nothing to commit is idle, and shall never fail for that. A failed writer shall refuse every mutation with the typed error `storage_unavailable`, grant no claim, keep supervising its runs and spooling their finishes (R129), and be reported by `daemon.status`, `orch status` and `orch doctor` (exit 1) from memory, since the attention queue needs the writer; only a restart shall end that state. (AP18) | Red team W3 and its residual W3-R1 (read to the letter, "60 s without a commit" failed an idle daemon) | must | `sqlite_writer::a_busy_database_is_retried_and_nothing_is_dropped` (a second connection holds the write lock), `::a_withdrawn_request_is_never_committed`, `::a_full_disk_is_retried_and_then_fails_the_writer`, `::a_corrupt_database_fails_the_writer_at_once`, `::an_idle_writer_never_fails`, `crates/orch-daemon/tests/daemon_storage_failure.rs::a_failed_writer_refuses_mutations_and_is_reported`; both |
| R129 | Before a run's finish (status, result, usage, last event number, and the lineage and epoch of the run's lease) is queued for the writer, the daemon shall write it, synced, to `spool/finish/<name>` in the data directory, and shall remove the file after the commit. The name is the finish file name of "Filesystem names", computed from the project id and the run id with the label `orch-finish-file-v1`, and R101 holds for it whatever the two ids contain; the file is created and reached as R102 says and is at most 128 KiB, a result over 64 KiB being stored as a blob first (R97). When the write or its sync fails, the finish is not safe and the daemon shall not act as if it were: it shall keep the finish in memory, try the spool again at the intervals of R128, hand the finish to the writer all the same (a commit ends the need for the file), grant no claim while any finish is unspooled, and report the number of unspooled finishes in `daemon.status`, `orch status` and `orch doctor` (exit 1). `orch daemon stop` shall still stop; it shall name each run whose finish is neither spooled nor committed and say that the stop loses that result. That is the one loss this requirement allows while a daemon runs, and R41 then ends the run as one whose process is gone. At a start, recovery shall apply every spooled finish whose run is unfinished before it judges that run (R41), by R36's rule for lineage and epoch, so that the run is finished and not cancelled. A file that names no unfinished run, carries another lineage or fails its check shall be removed and counted. A backup and an export hold no spool. (AP18) | Red team W3 and its residual W3-R1 (the spool write can fail, most likely when the disk is full and the writer is retrying for the same reason); R86 | must | `crates/orch-engine/tests/leases.rs::a_spooled_finish_survives_a_stop_before_its_commit`, `::a_spooled_finish_is_applied_before_recovery_judges_the_run`, `::a_stray_or_damaged_spool_file_is_removed_and_counted`, `::a_finish_that_cannot_be_spooled_is_kept_reported_and_committed_when_the_writer_recovers` (the spool directory refuses writes; no claim is granted meanwhile), `crates/orch-daemon/tests/daemon_storage_failure.rs::stop_names_a_finish_that_is_neither_spooled_nor_committed`, `crates/orch-infra/tests/fs_names.rs::finish_file_name_vector`, `::the_finish_file_name_is_the_whole_hash_for_any_run_id` (every finish-file row of the vectors of "Filesystem names", as a name), `crates/orch-engine/tests/leases.rs::a_finish_is_spooled_and_removed_under_its_hash_name_for_any_run_id` and `::recovery_reaches_a_spooled_finish_under_its_hash_name_for_any_run_id` (the same rows through the daemon's own operations, in a data directory made for the test: for a running run stored under each row's ids, the finish path writes exactly `spool/finish/<the row's name>` and removes it after the commit, and a start that finds that file applies it and removes it; a test passes the ids and computes a name only to compare; each asserts that nothing else was created below the data directory, that nothing was created, changed or removed outside it, and that a canary outside it keeps its bytes and metadata); both |
| R130 | `orch db query` shall use a connection opened read-only for that command alone, never the writer and never a pooled connection, with an authorizer that denies by default from prepare to the last row: it allows reading the listed columns of the diagnostic views through those views and calling the functions listed in "`orch db` commands", and nothing else. The statement shall be at most 16 KiB; the result at most 100,000 rows and 16 MiB; the run at most 5 s, stopped by the progress handler; and the connection's run-time limits shall be: a string or blob 1 MiB, expression depth 64, compound selects 8, function arguments 8, a `LIKE` pattern 1,000 bytes, attached databases 0. | Security review S3; ADR 0017 decision 19 (`hooks`, `limits`) | must | `db_query::a_function_outside_the_list_is_refused`, `::stops_at_its_statement_row_byte_and_time_limits`, `::each_run_time_limit_is_set_on_the_connection`, `::the_query_connection_is_not_the_writer_or_the_pool`; both |
| R131 | In 1.1 an attention item shall hold no text from an agent or any other source outside the daemon: its title and body are fixed text for its kind plus values the daemon derived (sizes, counts, ids checked as ids). The remedy shown for an item shall be fixed text chosen by its kind, with the subject id checked again when it is shown. A later kind that quotes outside text shall store it apart from the daemon's own fields, bounded, with its author's kind, id and run, shall show it escaped and marked as quoted, and shall build any action from a closed set and a checked subject id, never from stored text. | Security review S5 | must | `crates/orch-engine/tests/attention.rs::items_hold_only_text_the_daemon_derived` (task titles, agent names and run output holding `$(touch canary); --yes`, control bytes and a forged instruction), `crates/orch-cli/tests/attention.rs::the_remedy_is_fixed_text_for_the_kind`; both |
| R132 | Every lease-bound operation (renew, release, event append, finish) shall carry the lineage and the epoch of its lease, and shall be refused as stale unless both equal the lease's. A call refused for an earlier lineage shall change no row of any table (R36 says so for a finish; a renew, a release and an append store nothing either). After a restore a caller that holds a lease of the earlier lineage shall be refused each of them, across a restart of the daemon too. | Security review S6 and the re-review's condition C1; AP11; G9 as amended | must | `crates/orch-engine/tests/leases.rs::an_earlier_lineage_is_refused_renew_release_events_and_finish`, `::a_call_refused_for_an_earlier_lineage_changes_no_row` (each of the four calls, after a restore onto an empty directory and after a restart: every row of every table is identical before and after), `restore_fencing::an_earlier_lineage_is_refused_after_a_restart`, the bare filter `caller_selects_the_project_and_keeps_the_principal` (a task principal's lineage and epoch come back unchanged from `Storage::read` and `Storage::write`; the 3 entity references; this is the caller part only); both |
| R133 | `preapproval` shall be turned on by these paths and no other: `policy.set`; `orch init`; `orch import` of a 1.0 directory whose agents use `auto` (B14); and `orch import` of an export or `orch db restore` whose data has it on. Each shall print the trust statement. In agent context and in an MCP session `policy.set`, `orch import` and `orch db restore` are refused (R54), and `orch init` shall create the project with `preapproval` off and say that the owner turns it on. Every import and every restore that would turn it on shall name the policy in what the owner confirms (a typed answer or `--yes`; exit 2 without a terminal and without `--yes`): a source's agent files and an export's rows are input, and input alone shall not grant trust. No configuration or definition write shall change it. (AP5) | Security review S7; B14; AP5 | must | `ops_authorization::no_other_operation_sets_preapproval` (every operation of the catalogue, with a body that names the policy), `crates/orch-cli/tests/init.rs::init_in_agent_context_leaves_preapproval_off`, `import::a_source_with_auto_agents_needs_confirmation`, `db_export_import::an_export_with_preapproval_on_needs_confirmation`, `restore_fencing::restore_names_preapproval_in_its_confirmation`; both |
| R134 | A request key shall not apply to an operation that issues, rotates or enrols a credential, whatever its class: 1.1 has none, the catalogue's class test shall fail when one is added without this exclusion, a key sent with one shall be refused as `invalid_arguments`, and the client shall not resend it. A principal that is expired, revoked or not allowed the operation shall be refused before any stored result is looked up. A stored result shall be found only by its whole scope (identity, project, run, epoch, key). | Security review S8 | should | `ops_request_keys::a_key_is_not_shared_across_identity_project_run_or_epoch`, `::lease_fields_in_a_request_are_ignored`, `::a_refused_principal_gets_no_replay`, `crates/orch-proto/tests/catalogue.rs::credential_operations_are_excluded_from_request_keys`; both |
| R135 | When the directory that holds the nearest `.orch-project` lies below `.orchestry/workspaces/` of an ancestor whose own `.orch-project` holds the same id, the client shall take the ancestor as the project: a pointer that a workspace checkout carries is not a copy of the project (R34). | O1 (the owner's answer and its condition on worktrees); red team, out-of-lane item | must | `crates/orch-cli/tests/project_pointer.rs::a_pointer_inside_a_workspace_resolves_to_the_owning_project`, `::a_pointer_inside_a_workspace_of_another_project_is_still_a_copy`; both |
| R136 | The README upgrade notes and the manual shall say: that the 1.0 binary stays installed until the import has finished, with the exact command that installs 1.0 beside 1.1, which shall have been run as written (where an install method the README offers has no such command, the notes shall say so for that method and give the steps that keep the 1.0 binary before the upgrade, and the red team's finding I4 goes back to the owner); that an untouched copy of `.orchestry/` is taken before a refused source file is repaired, since that directory is the way back; what going back loses (everything done under 1.1) and that this grows with time; that a 1.0 daemon must not be started on an imported project, and what R121 does when one was. The RUNBOOK shall say never to open `orch.db` for writing and to use `orch db query`, that a reader outside the daemon holds the write-ahead log and that its file keeps its length until the daemon's connections close, what `storage_unavailable` means and what to do, and that a backup copied out of the data directory has left its protection. | Red team I2, I4, W3 and condition C3 of its closure; security review S4 | must | Fact check of each document against the code at the candidate commit, as R88; the install command run as written on a machine that has 1.1 installed, after which both binaries report their versions (recorded check on a Linux host). The same notes shall say that the release was qualified on Linux only and that macOS is untested until after 2.0 (record K) |
| R137 | Every call of the `wal.checkpoint` job shall be recorded with what started it (`timer`, `size` or `shutdown`) and with one outcome, read from the call's result and its two frame counts, the frames in the log and the frames copied so far, against the counts before the call: `copied_all` (the call succeeded, copied at least one frame, and no frame of the log is left); `copied_part` (it succeeded and copied at least one frame, and frames are left, as when a read holds them back or commits arrived during the call); `nothing_to_copy` (it succeeded and the log held no frame that was not copied); `held_back` (it succeeded, copied none, and frames are left); `busy` (`SQLITE_BUSY` or `SQLITE_LOCKED`); `failed` (any other result). The frames that a call copied are the difference of its own counts within one generation of the log and never a running total; where that difference cannot be told, the outcome shall be recorded as `unknown` and no copy shall be credited. A completed checkpoint, for R126's timer, is a call that succeeded, whatever it copied and whichever rule started it: the 5 minutes count from its end, so a call that the size rule started moves the next timer call. A `busy` or `failed` call is not completed and moves no timer; the job shall try again at a later look, never sooner than 1 s after the call's end (the 1 s is proposed and not measured). At a look that finds the backlog at 64 MiB or over, the call is a `size` call even when the timer is due as well. No call shall be made to bring about an outcome or to learn the backlog (R126). | The method challenge MC2 (what "completed" means for the timer was not said, and a call that the size rule starts moves the timer); R126 | must | `sqlite_writer::a_checkpoint_call_is_recorded_with_its_trigger_and_outcome` (one case for each outcome, the counts injected), `::a_repeated_running_total_credits_no_second_copy`, `::a_size_call_moves_the_timer`, `::a_busy_or_failed_call_moves_no_timer_and_is_tried_again`, `crates/orch-engine/tests/maintenance.rs::a_due_timer_at_64_mib_is_recorded_as_a_size_call`; both |
| R138 | `orch db restore` shall change the data directory only by the steps of "`orch db` commands" ("How a restore replaces the state"), in their order: (1) before anything is written, the checks of R76, R78, R100 and R117, and free space of at least the backup's size by its manifest above the disk floor (B11); (2) the whole restored state built under a scratch name in the data directory: the database, copied and checked against the manifest and then given, in one transaction, its new lineage and generation (R77) with every lease of the backup ended and its run closed (R100), and with it the blobs, the repositories, an empty spool and the `generation` file, all of it synced and the database closed with no log left, and then the closed database file read once more, with nothing but staged members beside it, to hold that lineage, that generation and no active lease, and its SHA-256 taken before and after that reading and found the same; (3) a marker, `restore-in-progress`, that names the scratch directory, the directory for the replaced state, the direction `forward`, the SHA-256 of the staged database, and for each member of the state (the database, its `-wal` and its `-shm`, the blobs, the repositories, the spool and the `generation` file) whether the replaced state holds it and whether the staged state holds it; the directory for the replaced state shall be created and synced before the marker, and every version of the marker, the change of direction included, shall be published whole, by a temporary file created exclusively, 0600, written, synced and renamed onto the marker's name, with the directory synced, and never by writing in place; (4) every member of the replaced state moved aside, a `-wal` or `-shm` that only it holds included, and only when none is left at a live name the staged members moved in, by renames inside the data directory in the stated order, with the directory that a member left and the directory that it entered both synced after each; (5) the final layout checked (every staged member at its live name, every replaced member aside, no `-wal` or `-shm` at a live name, the bytes of the database at its live name hashing to the SHA-256 that the marker holds, and the `generation` file holding the marker's generation) and only then the marker removed, last. That check shall read files and shall not open the database with SQLite. While the marker exists no command, `--continue` and `--abandon` included, shall open a database at a live name with SQLite, so that no step of this program leaves a `-wal` or a `-shm` there and a stop inside the final check leaves nothing behind: the check is run again from its start. While the marker exists the daemon and every offline command except `orch db restore --continue` and `orch db restore --abandon` shall refuse to start, change nothing, exit 1 and name the marker and both commands. `--continue` shall finish the steps from whatever a stop left and give the state that an uninterrupted restore gives. It shall tell what is left from the marker's list of members and from where each member lies, shall finish moving every replaced member aside before it moves any staged member in, and shall never take the absence of a member that was not staged to mean that a staged member was moved in. A layout that the steps cannot have left, among them a `-wal` or `-shm` at a live name beside a staged member that was moved in, shall change nothing, keep the marker and be refused, exit 1. While the direction is `forward`, once the scratch directory has been removed the restore is complete but for the marker: `--continue` shall check the final layout and remove the marker and do nothing else, and `--abandon` shall be refused. That refusal holds under `forward` only. `--abandon` shall publish the direction `back`, as every version of the marker is published, before its first rename, put every part of the replaced state back byte for byte, and then remove the scratch directory, the empty directory for the replaced state and the marker, in that order. Under the direction `back`, `--abandon` shall first test whether the abandonment has reached its end, before any other check of the layout and before any rule on the scratch directory: every replaced member at its live name, no other member at a live name, and the directory for the replaced state empty or absent. When that holds it shall remove what is left of the scratch directory, unread, whether that is all of it, a part of it or nothing, then the directory for the replaced state if it is there, then the marker, and shall do nothing else; `--continue` stays refused under `back`. Either may itself be stopped and run again, and from every state that a stop of these steps can leave with the marker standing at least one of the two shall run to its end. What this requirement promises for a stop is promised for a stop of the process; after a loss of power it holds as far as the syncs had reached the medium, and the rest is the manual recovery ("What a sync promises"). A stop, a failed copy or a full disk before the marker shall leave the live state unchanged; a scratch directory without a marker shall be removed, unread, by the next command or start, and so shall a temporary marker file and an empty directory for the replaced state that have no marker. A marker that fails its check is still a marker: nothing shall start, both commands shall refuse and change nothing, and the refusal shall name the manual recovery that the RUNBOOK is to give. At no step shall a database stand at its live name with a backup's leases active, or beside blobs, repositories, a `-wal` or a `-shm` of the other state, without the marker. R117's clause for a workspace that already stands is not built before the fifth point of AP14 is ruled, and nothing here decides it. | The resilience review F1 (a restore stopped between its replacements left a database beside the other state's blobs with the lock released, and nothing stopped a start); the resilience review of `a914e78`, N1 to N3 (the marker was written in place, so a stop left one that no command could read; one directory of two was synced after a rename; an abandonment after the scratch directory was gone had nowhere to put a part); the security review of `a914e78`, S-R1381 (a continuation that read a missing staged `-wal` as moved in could leave a new database beside the old log); the resilience review of `3c50293`, N8 (the last check opened the database, and a stop inside it left a `-wal` at a live name that both commands refused), N9 with the security review's note on the same case (an abandonment stopped after it had removed the scratch directory was refused by both commands) and N12 (what a sync of the marker promises after a loss of power); G3, G9 | must | `db_backup_restore::a_restore_killed_at_every_step_leaves_a_refused_or_complete_state` (a child is stopped before and after every copy, sync, rename and removal and at each read of the final check; afterwards either the live state is unchanged, or the marker stands and a start is refused, or the restore is complete; and where the marker stands, `--continue` or `--abandon`, as the table of stops says, then runs to its end, so that a state which both commands refuse fails this test unless that table calls it unexpected), `::a_continued_restore_equals_an_uninterrupted_one` and `::an_abandoned_restore_leaves_the_replaced_state_byte_identical` (each also stopped at every step of its own), `::a_marker_is_whole_after_a_stop_at_each_step_of_its_publication` (between the create, the write, the sync, the rename and the directory's sync, for `forward` and for the change to `back`), `::a_restore_stopped_between_the_database_and_its_wal_and_shm_continues_and_abandons` (the replaced state has a `-wal` and a `-shm` and the staged state has none; stopped before and after each of the three renames), `::continue_moves_every_replaced_member_aside_before_it_moves_one_in`, `::an_unexpected_layout_keeps_the_marker_and_is_refused` (among its cases a `-wal` at a live name beside a database that was moved in), `::the_final_layout_and_lineage_are_checked_before_the_marker_is_removed`, `::abandon_is_refused_under_forward_once_the_scratch_directory_is_gone`, `::an_abandonment_stopped_in_its_cleanup_is_finished_by_abandon` (stopped with a part of the scratch directory removed, with all of it removed, and with the directory for the replaced state removed as well; each ends with no marker and the replaced state byte for byte, and `--continue` is refused throughout), `::a_restore_stopped_inside_its_final_check_is_finished_by_continue` (stopped at each read; the check has left no file, and `--continue` ends as an uninterrupted restore does), `::nothing_opens_the_live_database_while_the_marker_exists` (no `-wal` or `-shm` appears at a live name during a restore, a continuation, an abandonment or a refused start), `::the_staged_database_file_alone_holds_the_new_lineage_and_its_hash_is_in_the_marker`, `::continue_and_abandon_run_again_change_nothing_more`, `::a_marker_that_fails_its_check_refuses_every_command_and_changes_nothing`, `::a_leftover_temporary_marker_is_removed_unread`, `::restore_refuses_without_space` (free space one byte under the need; nothing is written), `::a_failed_copy_while_staging_changes_nothing`, `::the_staged_database_has_the_new_lineage_and_no_active_lease_before_the_swap`, `crates/orch-daemon/tests/daemon_data_dir.rs::a_start_is_refused_while_a_restore_marker_exists`, `crates/orch-cli/tests/db_offline.rs::offline_commands_refuse_while_a_restore_marker_exists`; both |
| R139 | `orch daemon stop`, and a signal that asks the daemon to stop, shall end the daemon within a bound and in this order: (1) admission closes: a new request is answered `shutting_down`, no claim is granted, and the maintenance timer starts no job; (2) the transaction in progress commits or rolls back, and a job in progress queues nothing more (R26); (3) the collectors hand over what they hold, and the writer takes what is queued in the order of its lanes until the drain's deadline; (4) at the deadline a transaction that is still being retried (R128) is rolled back and given up: a request is answered and never committed afterwards, a finish stays in its spool (R129), and then, once the writer has ended its last transaction and takes no other and no event is given a number any more, for every run with events handed over and not committed the first and the last of their numbers (R61 gives a number at handover) are recorded in `spool/events-at-stop`, from which the next start writes that run's row of `run_event_gaps` (R61) before the run's events are numbered further and before admission opens; (5) reads still open are interrupted, a checkpoint call in progress is left to return and no other is started; (6) the lock is released last, by the process ending. The file holds the lineage; one of another lineage, or one that fails its check, is removed and counted, as R129 says of a finish file. Every version of the file shall be published whole: written to a temporary file created exclusively, 0600, in `spool/`, synced, renamed onto `spool/events-at-stop`, and `spool/` synced. A stop that finds a record of this lineage already there shall publish the ranges of that record together with its own and shall never overwrite or drop a range that an earlier stop recorded. A start shall write the gap rows of every recorded range in one transaction, shall write no second row for a range that a gap row already covers and no row over a number that a committed event of that run holds, cutting a recorded range to the numbers that neither holds, and shall remove the file only once that transaction is durable against a loss of power under the settings in force (R126): it was committed with `synchronous = FULL`, or a checkpoint that started after it has succeeded with no frame of the log left (R137, `copied_all`). A commit at `synchronous = NORMAL` alone is not that. A start that ends before that point shall leave the file as it was. Events that are neither committed nor recorded there, because that write failed as well, shall be named by run in the stop's report, as R129 names a finish. The stop shall end at its bound whatever is still in progress, and that shall lose nothing that is committed, spooled or recorded. What is said here of a stop that is killed is said of a stop of the process: after a loss of power the record can be absent or fail its check, and its ranges are then lost, with a count at most ("What a sync promises"). Proposed and not measured: 10 s for the drain, counted from the request; 5 s more for the reads and the checkpoint call; 20 s for the whole stop. This requirement covers a stop that was asked for and changes nothing in what a stop does to a run's process. | The resilience review F2 (the answers to O5 and D4 name a bounded shutdown, and no requirement stated its order, its bound or what becomes of queued events); the resilience review of `a914e78`, N4 and N5 (a record removed before its gap row was synced lost the gap, and a second stop overwrote the first record); the resilience review of `3c50293`, N11 and N12 (nothing said that the ranges are taken after the writer's last commit, so a gap row could cover an event that exists; and the record is synced by this program and not by SQLite); O5, D4 | must | `crates/orch-daemon/tests/daemon_storage_failure.rs::stop_is_bounded_while_the_writer_retries` (a second connection holds the write lock; fake clock), `::stop_follows_its_order` (each step is recorded), `crates/orch-engine/tests/run_events.rs::events_queued_at_stop_are_committed_or_left_as_a_gap`, `::a_gap_recorded_at_stop_is_written_at_the_next_start_before_new_numbers`, `::events_neither_committed_nor_recorded_are_named_by_the_stop`, `::a_second_stop_keeps_the_ranges_of_the_first` (the first record is not yet consumed), `::the_stop_record_is_removed_only_after_its_gap_rows_are_durable` (the removal is refused to a fake that reports the commit as not yet synced), `::a_start_repeated_after_a_kill_writes_each_gap_once`, `::a_start_that_fails_leaves_the_stop_record_unchanged`, `::a_stop_record_is_whole_after_a_kill_at_each_step_of_its_publication`, `::the_ranges_of_a_stop_are_taken_after_the_writers_last_transaction` (a commit is made to land as late as the order allows; no recorded range holds a committed number), `::no_gap_row_covers_a_committed_event` (a record is planted whose range holds committed numbers; the rows written cover the other numbers only); both |
| R140 | The failure policy shall cover every connection that the daemon holds on the live database, by the table of "Writer lanes" ("When another connection fails"). `SQLITE_CORRUPT` or `SQLITE_NOTADB` on any of them (a pooled read, the checkpoint connection, the query connection of R130, the connection a backup or an export reads through) shall fail the writer at once, as R128's fatal class does, with the connection and the result named, and no checkpoint shall be called afterwards. A corrupt backup, export or import file is that command's own refusal and is not this. Any other failed result on those connections shall be returned or recorded where it occurred and counted by kind of connection, the connection shall be opened afresh, and the writer shall not fail for it. `daemon.status`, `orch status` and `orch doctor` (exit 1) shall report, from memory: three checkpoint calls in a row that ended `busy` or `failed` (R137); a backlog that has stayed over R127's 256 MiB for more than 5 minutes, with the backlog, the time, the age of the oldest read and the last call's outcome; three failed reads in a row on the pool; and the last failed backup, export or query with its result. A report shall end when its condition does. The counts and the times are proposals that no measurement backs. | The resilience review F3 (R128 classed the writer's transactions alone: corruption first seen by a checkpoint was a recorded figure while the writer went on committing, and a checkpoint that failed every time showed only when the disk floor tripped); D4 | must | `sqlite_writer::a_corrupt_result_on_the_checkpoint_connection_fails_the_writer`, `::a_corrupt_result_on_a_read_connection_fails_the_writer`, `::a_corrupt_result_on_the_query_or_backup_connection_fails_the_writer` (the result is injected at the connection named), `::other_failures_off_the_writer_are_counted_and_do_not_fail_it`, `crates/orch-daemon/tests/daemon_storage_failure.rs::three_failed_checkpoints_in_a_row_are_reported`, `::a_backlog_over_its_ceiling_for_five_minutes_is_reported` (fake clock), `::a_report_ends_with_its_condition`, `crates/orch-cli/tests/db_backup_restore.rs::a_corrupt_backup_file_fails_the_command_and_not_the_writer`; both |

For the implemented R8–R15 and R97 reference checks, the acceptance cells above
name Cargo substring filters, not Rust module paths prefixed by the test binary's
name. The plan's **T8 reference verification crosswalk** fixes the command,
registration counts and remaining consumers. A successful command with zero
selected tests is no evidence. These checks do not complete SQLite, runtime
finish or import acceptance. Linux is the release gate through 2.0 by the owner's
2026-11-23 direction; macOS compatibility belongs to After 2.0.

## Storage port v2

The contract of deliverable F. Rust signatures are fixed by the port PR and recorded in `docs/TRAITS-FROZEN.md`.

| Name | Meaning |
|---|---|
| `Caller` | `{ principal, project }`, passed to both entry points. `Principal` is `Owner { identity }`, `Worker { identity }` or `Task { identity, task, run, lineage, epoch }`. The lineage is the restore lineage of R77; a caller holds it with its epoch (R132). |
| `Storage::read(caller)` | Returns a `ReadSnapshot`: every read through it sees the same committed state. |
| `Storage::write(caller, lane, work)` | Runs `work` on a `UnitOfWork` in one transaction on the writer. An error from `work` rolls everything back. `lane` is `Claims`, `Entities`, `Events` or `Maintenance`. |
| `Revision` | Positive integer on every entity. `create` stores 1. `update(entity, expected)`, `delete(kind, id, expected)` and `move_to_end(kind, id, expected)` compare `expected` with the stored value and fail with `StaleRevision { kind, id, expected, actual }`. |
| `PageRequest` | `{ limit, cursor, sort, with_count }`. `limit` defaults to 100; 0 or more than 1,000 is refused. |
| `Page<T>` | `{ items, next_cursor, total }`. `next_cursor` is absent on the last page. `total` is present only when `with_count` was set. |
| `Cursor` | Opaque text of at most 512 bytes: the sort, the last row's sort values and id, and a hash of the filter. It holds no snapshot. |
| `position` | Explicit order (R11). `move_to_end(kind, id, expected)`, which also raises the revision, is its only other writer. |
| `claim_lease(new, now_ms)` | Inserts a lease only if the task has none and the agent has no active one, and gives it the task's next epoch. Returns `Granted(lease)` or `Held(lease)`. `now_ms` is UTC milliseconds from `Clock::now_ms()`; `expires_at` is checked `now_ms + LEASE_MS` (`LEASE_MS: i64 = 120_000`). |
| `renew_lease`, `release_lease` | Renew takes `(task, lineage, epoch, now_ms)` and sets the same checked millisecond deadline as claim; release takes `(task, lineage, epoch)` without a clock input. Both refuse with `StaleEpoch` unless lineage and epoch are the lease's (R132). |
| `append_events(run, lineage, epoch, batch)` | Refused with `StaleEpoch` unless lineage and epoch are the lease's. Inserts with "ignore if `(run, seq)` exists". Returns `{ stored, skipped }`. `record_event_gap(run, after_seq, dropped)` records a loss keyed by `(project_id, run_id, after_seq)`. Replaying an identical count is a no-op; a conflicting count returns `InvalidEvents` and writes nothing, even if the caller catches the error and commits. |
| `read_events(run, after_seq, limit)`, `read_events_tail(run, count)`, `prune_events(run, max_rows)` | Range read, tail read, and a bounded delete that reports whether the run is finished. |
| `put_retry`, `get_retry` / `read_retry`, `delete_retry` | One project-local row per task: `task`, `attempt` (at least 1), `due_at` (nonnegative UTC milliseconds), `error` (opaque text, whole: ADR 0020 stores more than 65,536 UTF-8 bytes through the blob store, and more than 64 MiB as ordered chunks). Put replaces the whole row; get is a point read. |
| `due_retries(now, limit)` / `read_due_retries` | Rows with `due_at <= now`, ordered by deadline then UTF-8 task bytes; explicit limit 1..=1,000. This read order is not scheduler dispatch order. |
| `list_retries(after, limit)` / `read_retries` | Complete enumeration, including future rows, in the same order. Keyset cursor `(due_at, task)` is exclusive; pages are 1..=1,000 and `next` exists exactly when more rows remain. There is no total cap or deletion. One read snapshot gives one committed view throughout a walk; a unit of work sees its own writes. A cursor can outlive its row but switching snapshots does not preserve an atomic walk. |
| `add_intent(payload, now_ms)`, `pending_intents(after, limit)`, `finish_intent(id, now_ms)`, `delete_finished_intents(now_ms, max_rows)` | Side effects (R28); the T8c intents amendment below states the typed payloads, the lease boundary and the retention. |
| `check_request_key(scope, op, hash)`, `store_request_key(scope, op, hash, result)` | Return `New`, `Replay(result)`, `ReplayWithoutResult` or `Mismatch`. |
| `open_attention`, `ack_attention`, `resolve_attention` | The attention queue; `open` is a no-op when an open item exists for the subject. |

T8c contract amendment (2026-11-22): the registered `memory_leases_v2` and
`memory_operations_v2` modules run backend-neutral lease and combined scenarios
through factories with an explicit restore hook. The latter covers entity/event/
lease rollback, same-transaction release fencing, restore fencing and injected
clock jumps. `memory_events_v2::repeated_gap_is_idempotent_and_conflict_changes_nothing`
pins gap identity. The file reference registers these same suites
(`file_events_v2`, `file_operations_v2`, `file_leases_v2`); SQLite must register
them when wired. Two-backend acceptance of the operational port is therefore
unproven until SQLite is registered, and a real suspend and resume on Linux is unproven.
R42's real-suspend check (pending, issue 110) and runtime renewal ordering remain integration checks.

T8c intents amendment (2026-11-22): the port `orch_core::store::intents` adds
`IntentRead` to `ReadSnapshot` and `IntentWrite` to `UnitOfWork`, with the same
two references (`memory_intents_v2`, `file_intents_v2`) running one shared
suite. Signatures, with `now_ms` UTC milliseconds from the caller's clock (the
port reads none):

- `add_intent(&IntentPayload, now_ms) -> Intent`; `finish_intent(id, now_ms) ->
  FinishOutcome`; `delete_finished_intents(now_ms, max_rows) -> usize`;
  `get_intent(id)` and `list_pending_intents(after, limit)` in a unit of work;
  `read_intent(id)` and `read_pending_intents(after, limit)` on a snapshot.
- `IntentPayload` is closed: `process.start { run }`, `process.stop { run }`,
  `workspace.remove { task }`, `blob.delete { hash }`. A run or task is one name
  of 1 to 128 bytes of `A-Z a-z 0-9 . _ -` other than `.` and `..`; a hash is 64
  lowercase hex digits. Anything else is `InvalidIntent` and changes nothing, so
  no row holds a path (R105, R106). The spec's event-file form of `blob.delete`
  is not in 1.1 (owner decision of 2026-11-21). R118's check that a directory is
  the task's stays the performer's.
- Ids are project-local, start at 1, grow by one and are never reused, even
  after retention emptied the table. `pending` returns undone rows with an id
  above `after`, lowest first, at most `limit` (1 to 1,000), so a backlog of any
  size is enumerated by resuming from the last id.
- Completion is idempotent and the first time wins: a second `finish`, or one
  for a row that retention deleted, is `AlreadyDone`; an id that was never
  issued in the project is `InvalidIntent`. Retention deletes finished rows
  whose `done_at` is at least 7 days (`DONE_RETENTION_MS`) before `now_ms`,
  lowest id first, at most 1,000 a call; pending rows are never deleted.
- Authority (R132): `process.start` and `process.stop` need the run of a lease
  that exists in the unit of work under the current lineage, and a task
  principal may name only its own run with its own lineage and epoch, else
  `StaleEpoch`. The lease is read when the intent is added, so a cancel (R37)
  adds the stop intent before it releases the lease. A restore ends every lease
  and so the right to add either kind; rows already written stay. `add_intent`
  carries no lineage or epoch of its own, because the lease in the same
  transaction is the fence, as for `finish` (R36).
- `IntentKind::repeats_after_restart` states the kind's rule: false for
  `process.start` only. The port keeps a `process.start` pending until recovery
  (R41, T13) finishes it, and never performs anything; intents of an earlier
  generation are recovery's to judge, as the port has no generation.
- A callback that catches a refusal and commits leaves no partial change: every
  check precedes the first write, and an id is spent only by a stored row.
- The performers, recovery, the `intents.perform` job and the SQLite table
  (`intents` in the schema) are T13, T15 and T10's.

T8c attention amendment (2026-11-22): `orch_core::store::attention` is an
inactive operational port, registered by `memory_attention_v2` and
`file_attention_v2` against one shared suite (R8, R81–R84, R131).

- A session with the reserved empty `Caller.project` names daemon-wide attention
  (`project_id = ''` in the schema): only `disk_floor` is allowed there. A
  nonempty project session permits only `dispatch_off_with_work`; it never reads
  or changes daemon rows or another project's rows. A health consumer explicitly
  reads both scopes using separate sessions, which are separate snapshots.
- Inputs are closed typed facts: disk subject (data directory or checked project
  workspace id), free/floor bytes, waiting count and oldest waiting UTC-ms time.
  Title, detail and remedy are kind-controlled prose; ids are rechecked for display.
  No agent text, executable action or caller-supplied provenance is stored.
- `open_attention(facts, at)` returns an existing open kind/subject unchanged,
  including its facts and revision. Reopening after resolution allocates a fresh
  monotonic scope-local id, bounded by SQLite's signed integer range and mapped
  to decimal TEXT. Retention never reuses an id. Revisions begin at 1.
- `ack_attention(id, expected, at)` records the first identity from
  `Caller.principal` and UTC-ms time without resolving. `resolve_attention`
  records the first closed kind-compatible reason/time. Both check the expected
  revision, increment only on actual change and preserve first provenance on a
  current-revision repeat. Invalid or backward times are refused without changes.
- `read_attention`/`get_attention` read one id; snapshot `read_attention_page`
  and transaction `attention_page` enumerate All/Open/Resolved rows in ascending
  id order using pages of 1–1,000. Last-id cursors bind scope and filter and retain
  no snapshot. `expire_attention(cutoff, limit)` removes at most 1,000 resolved
  rows in id order with `resolved_at < cutoff`; equality and open items survive.
  T15 computes the cutoff from the configured retention (default 90 days).
- Both references buffer attention with all other writes for rollback and snapshot
  isolation. File loads validate scope/facts and default absent legacy attention
  to empty. Shared tests cover transitions, stale/caught errors, rollback, snapshots,
  concurrency, paging beyond 1,000, isolation, fixed content and the restore fixture;
  file tests cover reopen, legacy data, counters and corrupt/scope-invalid loads.
- Authorization remains dispatcher-owned: only daemon work opens/resolves/expires;
  acknowledgements require the local owner path under R54. SQLite (T10), runtime
  evaluation/retention (T15/T19), protocol/authorization (T17/T18) and doctor/CLI
  are unwired. These reference tests do not complete R81–R84's runtime acceptance
  or durable backup/restore (T23), and do not release T8c integration.

T8c request-keys amendment (2026-11-22): the port `orch_core::store::request_keys`
adds `RequestKeyWrite` to `UnitOfWork`, with the same two references
(`memory_request_keys_v2`, `file_request_keys_v2`) running one shared suite. It
refines the table row for `check_request_key` and `store_request_key`; no
protocol, hashing, resend or dispatcher is added. `now_ms` is UTC milliseconds
from the caller's clock; the port reads none.

- `RequestScope::of(&Caller, key)` is the only way to make a scope. Its identity
  and project are the caller's; its run and epoch are the held lease's for a task
  principal and empty and 0 for an owner or worker session (the spec's owner
  rule). A request supplies the key and nothing else of the scope. A task
  principal with an empty run or epoch 0, an empty identity or project, or a key
  that is not 16 to 128 characters of `A-Z a-z 0-9 _ -` is `InvalidRequestKey`.
  A unit of work refuses a scope built from another caller, so another identity,
  project, run or epoch is never looked up.
- `check_request_key(&scope, op, req_sha256, now_ms) -> KeyCheck` reads only and
  sees the unit of work's own stores: `New`, `Replay(result)`,
  `ReplayWithoutResult` or `Mismatch`. `op` is 1 to 128 bytes of
  `A-Z a-z 0-9 . _ -`; `req_sha256` is 64 lowercase hex digits computed by the
  caller (the canonical form is T17's). A live row with another `op` or hash is
  `Mismatch`, whichever differs.
- `store_request_key(&scope, op, req_sha256, result, now_ms)` stores the row in
  the unit of work that makes the change, `expires_at = now_ms + 3,600,000`. It
  never replaces a live row, whatever it holds: storing over one, an invalid
  input or a scope of another caller is `InvalidRequestKey` and changes nothing,
  even when the callback catches it and commits. The caller checks first and
  stores only on `New`; two concurrent repeats run on the one writer in turn, so
  the second finds the first's row.
- A row is replayable while `now_ms < expires_at`, so at exactly one hour it is
  already gone. An expired row is as absent as a deleted one: `check` gives
  `New` and `store` replaces it, before the expiry job has run.
- A result of at most 65,536 bytes (bytes, not characters) is kept whole; a
  longer one, whatever its text, keeps no body and no prefix
  (`result_truncated = 1`), and its replay is `ReplayWithoutResult`. The first
  outcome stands: a replay never re-evaluates the change.
- `delete_expired_request_keys(now_ms, max_rows)` deletes rows with
  `expires_at <= now_ms`, earliest expiry first, ties by scope, at most
  `max_rows` (1 to 1,000), and returns the count; only the caller's project. It
  is the body of the `request_keys.expire` job (T15).
- Authorization comes first, in the consumer (R134). The port authorizes nothing.
  The dispatcher must refuse an expired, revoked or disallowed principal, and an
  operation that issues a credential, before it builds a scope or calls
  `check_request_key`, so that a refused principal gets no stored result. A task
  principal's run and epoch are taken from the lease the daemon authenticated for
  the connection, never from a request field.
- Request keys have no snapshot read: no read path may show a stored result
  (R74), and the only reader is the writer, in the transaction of the change.
  Rows are not exported or imported (R114). A restore ends leases but deletes no
  key; epochs are not reused, so a later run never matches an earlier one's rows.
- A negative `now_ms` is refused as `InvalidRequestKey` on the lookup path as it is on the store path, and the lookup
  changes nothing (`input_outside_the_shapes_is_refused_and_changes_nothing`, registered as `memory_request_keys_v2` and `file_request_keys_v2`).
- The scope has no principal-kind field. An owner session and a worker session of one identity and project share a
  scope (run empty, epoch 0), and a task principal differs from them only by its run and epoch. The dispatcher
  derives a principal's kind from its persisted identity (T17, T18).
- Persisted rows of the file reference are checked on load as the port would have written them: sorted by identity,
  run, epoch and key without a repeat; a key, operation and hash of the port's shapes; a result within the bound; an
  identity that is not empty; a run that is empty exactly when the epoch is 0; an epoch of at most `i64::MAX`, the
  ceiling `RequestScope::of` applies to a task principal, so that a stored row never names an epoch that no scope can;
  and `expires_at` exactly `created_at + 3,600,000`. A document that fails any of these is refused whole, with its path and the
  reason (`a_hand_edited_request_key_outside_the_contract_is_refused_on_read` names the cases, including an epoch above
  `i64::MAX`, which is refused as an impossible scope).
- Unknown fields in that document. A request-key row denies them: a field the row does not define refuses the
  document. The collection that holds the rows ignores a field it does not know, so a document that carries an extra
  field there still loads (`retry_existing_single_object_encoding_and_wrapper_extensions_remain_readable`); the type
  has no place for such a field, so a rewrite of the document writes only `rows` and does not keep it.
- TTL compatibility. The persisted check above requires an expiry of exactly one hour after creation, so the one-hour
  `KEY_TTL_MS` is part of what the reference document accepts. A change of the TTL must migrate or reconcile the rows
  already stored (or version the check) in the same change; otherwise a document that holds an earlier row is refused
  on load. No such change is made or planned in 1.1, and no migration exists.
- Several rows may share one expiry. `delete_expired_request_keys` removes them earliest expiry first and ties by
  scope, an order the port does not let a caller see. The reference tests do not prove that order on SQLite: the
  full-scope tie order in the SQLite adapter is T10's, tracked in
  [issue 127](https://github.com/yuxhuang/r-orch/issues/127) (issue 109, PR 171 follow-up N1).
- `state = 'in_flight'` and `commit_seq` stay unwritten in 1.1. The SQLite table
  (`request_keys` in the schema) and the `request_keys.expire` job are T10's and
  T15's.

Each list has a fixed set of sorts. The default sort of a list is its 1.0 order, served by an index, and
deliverable B confirms each line. Every list also offers the sort `id`, whose values never change (R49):

| List | Default order | Other sorts |
|---|---|---|
| tasks | status rank, `updated_at` descending, `position` | `position`, `priority`, `created_at`, `updated_at` |
| agents | `position` | `name` |
| goals | status rank, `updated_at` descending, `position` | `created_at` |
| messages | `created_at`, `position` | none. `Filter::PendingMessages` (ADR 0020) lists with this default order |
| context entries | `key` | none |
| teams | `created_at`, `id` (1.0's order came from the filesystem and was not a contract) | `name` |
| runs | `started_at` descending, `id` (1.0 broke equal start times by directory order, which was not a contract) | per task: `attempt` (`Sort::Attempt`: `Run.attempt` numerically descending, then id bytes ascending; only for `Filter::Runs` with a task, ADR 0020). `Run` has no `run_no` field |

The engine calls `move_to_end` only at the call sites deliverable B lists as observable. Until that list
exists the rule is: wherever 1.0 saved a task, agent, goal or message.

Removed with port v1: `StateStore` (replaced by leases, retries, counters and project settings), the rule that
every YAML read resolves merge keys (only the importer reads YAML), and the reorder inside `save`.

Retry reference boundary. On `main` at `07b5f967`, the baseline, the memory and file references accepted error text
up to 65,536 bytes and refused larger values without truncation. R97 is binding and ADR 0020 settles how it is met: a
retry keeps its whole `error` (see "T8 consumer-contract amendment" below). The refusal was an implementation gap,
not an exception, and the change that carries this revision ends it on both references.
The 1.0 enqueue was first-wins (call `get_retry` before put), malformed legacy
retry deadlines count as due, and dispatch uses candidate order and agent rotation,
not deadline order. T12/T14/T21 retain these contracts. Complete enumeration is
checked on both references by `the_complete_read_pages_every_row_including_future_ones_in_read_order`,
`a_snapshot_walk_ignores_commits_made_between_its_pages`, and
`a_restore_keeps_every_retry_row` in `storage_contract/retries_v2.rs`.

T8 consumer-contract amendment (issue 109, 2026-11-23; [ADR 0020](../decisions/0020-storage-consumer-contracts.md)).
The three contracts below are decided and are implemented by the change that carries this revision, on the memory
fake and the file reference. `main` at `07b5f967`, the baseline, has neither new variant and refuses a retry `error`
over 65,536 bytes; that is what a reader of `main` before the change merges sees. They are reference-port contracts:
nothing in them is wired into the daemon, and none accepts T10, T12, T14, T21 or T23 at runtime.
`docs/TRAITS-FROZEN.md` records them with the signatures of the port.

- **Pending messages.** `Filter::PendingMessages { recipient: String, now: String }` selects a message whose status
  is `pending`, whose `to_agent_id` is `Some(recipient)` byte for byte (a broadcast with no recipient is never
  pending) and whose `expires_at` is absent or `expires_at >= now` as a string comparison. 1.0's `is_expired` is
  `expires_at < now`, so equality is pending, and an `expires_at` that is not a date is compared as text, never
  parsed; the port adds no validation of `now`. `now` is the caller's captured `Clock::now_iso()`: the port reads no
  clock. The filter runs before the count, the cursor and the page bound, so a page returns up to `limit`
  matches however many rejected rows precede them, and `total` counts matches only. A cursor binds the recipient,
  `now`, the sort and the project, so another `now` or recipient is refused. The order is the messages' default.
  Tests, required on both references and on the SQLite adapter (T10): exact recipient, status, expiry equality, an
  expired and an unexpired row, a null recipient, an invalid expiry, a match after more than one page of rejected
  rows, the count, a later commit that a snapshot walk does not see, the cursor's binding, and another project. The
  references carry them in `pending_messages_filter_status_recipient_and_lexical_expiry` (status, exact recipient
  including case and prefix, a null-recipient broadcast, absent, equal, future, past and non-date expiries, an empty
  `now`, the count, another project) and `pending_messages_page_matches_before_count_cursor_and_bound` (1,100
  rejected rows before three matches, the count, a unit of work that sees its own writes, a snapshot walk that a
  committed insert and a status change do not disturb, and a cursor refused for another recipient, another `now`,
  another filter kind, another sort and another project). Each is registered as `memory_v2`, `queued_memory_v2` and
  `file_v2`.
- **Per-task run order.** `Sort::Attempt` is offered only for `Filter::Runs { task: Some(_), .. }`. It orders by
  `Run.attempt` as a number, descending (10 before 2 before 1, and `u32::MAX` first), and breaks ties by the run id's
  bytes ascending. It is refused as `InvalidPage` for `Runs` with no task, for any other family and for any other
  filter, before paging. `Sort::Default` for runs stays `started_at` descending then id, so no caller changes.
  The port's number is `Run.attempt`: `Run` has no `run_no`, and the schema's `run_no` (a per-task start order) and
  `attempt_no` columns are T10's. For an imported run whose 1.0 attempt repeats within its task (stored uncounted,
  "Import rules"), the adapter must still return and sort by the 1.0 number it returns as `Run.attempt`; which
  column holds it is open for T10 and is not decided here. Tests, in the same three registrations:
  `run_attempt_order_is_numeric_task_scoped_and_keeps_default` (10 before 2 before 1, `u32::MAX` first, equal attempts
  by id, another task and another project excluded, an `agent` filter, and `Sort::Default` unchanged),
  `run_attempt_pages_keep_snapshot_and_bind_every_query_field` (a snapshot walk that a delete and an insert do not
  disturb, the count, and a cursor refused for another task, another agent filter, another sort and another project)
  and `run_attempt_sort_refuses_every_unscoped_or_other_family_filter` (`InvalidPage` for runs with no task, for
  every other family and for `PendingMessages`, with or without a cursor).
- **Whole retry text.** `Retry.error` is the complete logical text, including for a retry with no surviving run (R97,
  decision 7, ADR 0017 decision 11). Text of at most 65,536 UTF-8 bytes (bytes, not characters) may stay in the
  row. Longer text is published to the blob store, synced, before the row naming its hash is committed, and a read
  returns the original text byte for byte. A failed publication, or a missing or corrupt blob when writing, fails the
  write, and the previous row is exactly as it was. A reference that does not resolve on read is an error from every
  retry read, never an empty, shortened or partial row. A read resolves only the rows it returns, so a damaged row that a
  due read's deadline or a page's limit excludes does not fail it, and the file reference and the disk-backed fake
  run the three async snapshot reads' blob I/O on the blocking boundary, off the executor. Nothing truncates. `Retry::validate` no longer refuses long
  text and keeps its other checks; the first-wins `get_retry` before `put_retry`, the orders, the 1..=1,000 bounds and the
  absence of any cap on rows are unchanged. A capped diagnostic with the full text on the run row would amend R97
  and is the owner's to adopt; this amendment does not.

  *Above one blob object.* `DiskBlobStore` accepts and returns at most 67,108,864 bytes (64 MiB) per object, and
  that bound stays. Text of 65,537 to 67,108,864 bytes inclusive is one object named by the row's hash. Text of more
  than 67,108,864 bytes is stored as an ordered mapping to chunks, each an existing blob object of at most 67,108,864
  bytes, and the row keeps the SHA-256 of the whole value, which no single object holds. The mapping records
  position, so identical chunks are listed once per position. The split is by bytes: a character may straddle a
  boundary, and the joined bytes are decoded only after the whole-value hash matches. A read verifies every object,
  joins them in order and verifies the whole-value hash; a missing, corrupt or reordered chunk is an error, and no
  partial value is returned. A write publishes every object before it commits the row and its mapping together; if a
  later chunk fails after an earlier one was published, the previous row stays readable, even when the caller
  catches the error and commits other work in the same unit of work. Objects an interrupted write leaves behind
  follow the blob store's lifecycle. Rows that already hold one object stay readable. The mapping's physical encoding
  belongs to the adapters, not to this spec, and size alone never refuses a valid diagnostic. This is a decision of
  2026-11-23 and ships no measurement above 64 MiB.

  *The reference representation* (ADR 0020 decision 4 has the detail). The mapping is `{sha256, chunks}`: the SHA-256
  of the whole UTF-8 value and the ordered, per-position list of chunk hashes. The file reference stores a retry's
  `error` in its project document as an inline string, as `{"sha256": H}` for one object, or as
  `{"sha256": H, "chunks": [...]}` above 67,108,864 bytes, and cuts chunks at 67,108,864 bytes. The memory fake is
  *logical* by default (whole strings, no blob, no claim about backing) and *disk-backed* when built with
  `FakeStorage::with_retry_blob_store`, which publishes through a `BlobStore` and keeps a hash and chunk list for any
  text over 65,536 bytes. No public type of the port carries the mapping. The file reference and the disk-backed fake
  publish each object and read it back before they stage the row, so a failed publication leaves the staged row untouched, and objects left by a
  rolled-back unit or a replaced row are unreferenced until the blob store's sweep.

  Tests required on both references, with the test that carries each item. The first three rows are scenarios of the
  conformance suite's `retry_suite` (21 scenarios), registered as `memory_retries_v2` (logical fake),
  `blob_memory_retries_v2` (disk-backed fake) and `file_retries_v2`, so 3 registrations each. The next ten rows are in
  `storage::v2::tests` of `orch-infra` and run the file reference and the disk-backed fake unless noted. The last row
  is a private unit test of `orch-core`. The logical fake keeps whole strings and has no object to damage, so a row
  that is about publication or damaged references is never carried by it.

  | Required | Test |
  |---|---|
  | Inline at 65,536 bytes and a blob at 65,537; a multibyte text that crosses the 64 KiB line by bytes and not by characters; a round trip with no run (no task or run entity exists); a restore | `retry_whole_text_round_trips_at_the_utf8_boundary_without_a_run` (conformance, 3 registrations; it models two enqueue calls with a first-wins `get_retry` guard inside one unit of work, and the production caller stays T12's) |
  | A text of 67,108,865 bytes (one above one blob object), stored with no run entity, then a fixture restore to a new lineage (`restore_to_lineage`: it ends leases, switches the lineage and keeps counters; it is not a backup copy); `read_retry`, `read_due_retries` and a complete walk return it whole | `retry_chunked_text_survives_restore_without_a_run` (conformance, 3 registrations; on the logical fake the text is a whole string, so only the two backed registrations exercise chunks) |
  | A rolled-back replacement and a committed one; a held snapshot that keeps the original; then a replacement of the long text by a short one (9 bytes), read by `read_retry`, `read_due_retries` and a complete walk, and of that by a long one (80,000 bytes), read by `read_retry` | `retry_whole_text_replacement_rolls_back_and_snapshots_keep_original` (conformance, 3 registrations) |
  | The file reference's backing: no `blobs` directory for inline text; the complete bytes exist before the row does; the row holds `{"sha256": H}`; reopening returns the whole text on every read | `retry_text_boundary_is_backed_before_the_hash_row_and_reopens_whole` (file reference only) |
  | One object at 67,108,864 bytes and chunks at 67,108,865; a character straddling the chunk boundary; the joined bytes equal the whole-value hash; a damaged tail and then a missing tail each an error on all six reads (`read_retry`, `read_due_retries`, `read_retries`, `get_retry`, `due_retries`, `list_retries`), on both references; on the file reference, a reordered mapping an error on the same six reads | `retry_object_limit_is_not_a_diagnostic_cap_and_chunk_order_is_verified` |
  | Identical chunks at two positions (the file reference's mapping has three entries) | `retry_repeated_chunks_reconstruct_every_occurrence_on_both_references` |
  | A failure on a later chunk after an earlier one was published; the earlier row readable after the caller catches the error and commits other work; the earlier object still present | `retry_later_chunk_failure_keeps_prior_row_and_earlier_published_bytes` |
  | A publication whose object is missing or corrupt on read-back, each leaving the earlier row intact even when the caller catches the error and commits | `retry_publication_refusal_preserves_prior_rows_even_when_caught_on_both_references` |
  | A missing and a corrupt blob after commit each an error on `read_retry`, `read_due_retries`, `read_retries`, `get_retry`, `due_retries` and `list_retries`, with the document unchanged and an absent task still reading as none | `retry_missing_and_corrupt_blobs_fail_all_read_paths_on_both_references` |
  | A rolled-back unit that published a 65,537-byte text: the earlier row stays, the rolled-back task reads as none, and the published bytes are still on disk at their hash path with no row naming them | `retry_rollback_keeps_published_bytes_without_a_reference_on_both_backed_references` |
  | A row that holds the single-object form `{"sha256": H}` (made by hand in the document, from a 65,538-byte text of two-byte characters put in the blob store; no earlier release wrote one) reads whole on all three snapshot reads after a reopen; the document is not rewritten by the reads; an extra field in the document's request-key collection wrapper does not prevent the reopen, and a request key stored before it still replays | `retry_existing_single_object_encoding_and_wrapper_extensions_remain_readable` (file reference only) |
  | A healthy row and a damaged row (blob removed) whose deadline is later or that sits past the limit: `read_due_retries` at a fixed `now` and at limit 1, `read_retries` for the first page, and `due_retries` and `list_retries` in a unit of work, return the healthy row; `read_retry`/`get_retry` of the damaged row and the page that reaches it fail; on a current-thread runtime a blob store guard fails a snapshot read that touches a blob on the executor thread | `retry_selection_does_not_resolve_excluded_damaged_rows_on_both_references` (both references) |
  | A hand-edited retry row whose stored text mapping, single (`{"sha256": H, ...}`) or chunked, carries an extra field: the document fails to load as a parse error and its bytes are unchanged | `retry_stored_text_unknown_fields_are_refused_for_single_and_chunked_objects` (file reference only) |
  | The disk-backed fake's private mapping, with two of its three chunks swapped: `get`, `due` and `page` of its private retry state, which every one of the six port reads calls, each fail with "reconstructed retry does not hash". Its blob store is a reversible text-name double (the content names the object), so the test proves the whole-value hash guard and not hashing or damage; the physical checks above use the real `DiskBlobStore` and SHA-256 | `store::retries::fake::tests::reordered_retry_chunks_fail_every_memory_retry_read` in `orch-core` (requires the `test-support` feature) |

  Not covered by any test of this change, and therefore open for the owner of the consumer or for a follow-up: the
  SQLite representation of any of these rows (T10 repeats the physical checks on SQLite); the blob store's sweep
  itself (the rollback test shows the unreferenced bytes survive the rollback, not that the sweep later removes them;
  its row holds one object of 65,537 bytes, not a chunk); a missing or corrupt chunk on the logical fake (it has none);
  a hardware or process crash between publication and commit; a restore that copies files (the restore of the
  chunked-text scenario is the fixture's lineage switch, not a backup, and T23 carries every reachable chunk, its order
  and the whole-value hash through backup, restore, export and import); and a 1.0 import (T21 rebuilds an imported 1.0
  error into the same representation). The single-object row of the last-but-one table row is a fixture of the
  documented encoding, not a row an earlier release wrote, because `main` at `07b5f967` refused the text.
  The file reference's reordered mapping is made by editing its document, and the disk-backed fake's mapping is
  private, so its reorder is proved by the unit test and not through the port. The expected counts of the runs that
  select these tests are in the plan's "T8 consumer contracts (issue 109)", and a result is a receipt of the PR that
  carries the change, not a statement of this spec.
- **Owners.** This change implements the three on the memory fake and the file reference and registers the shared
  tests on both. T10 owns the SQLite representation and the adapter registration. T12 owns the finish and
  enqueue caller (get, then put, in one unit of work), the retry row written with the status and the lease release,
  and lease-bound finish and restart fencing (R36, R132). T13 owns generations, recovery and the Linux
  sleep and wake check (R41, R42). T14 owns `retry_pending` gating and candidate-order dispatch. T21 owns the
  legacy mapping of an unparsable deadline to due, counted in the dry run, and the whole 1.0 error (R97). T23 owns
  durable restore with a new lineage (AP14 is open) and the whole-retry export and import round trip. T27 owns the
  second-host peer check (R89). The plan's "T8 consumer contracts (issue 109)" lists the verification names and the
  follow-ups N-a to N-c.

## Schema for 1.1

The starting point is the 2.0 sketch attached to the goal: 50 tables and one FTS5 index. Times are UTC
milliseconds; the API keeps ISO-8601 strings. Recommendation for O2: 1.1 creates 33 tables.

| Group | Created in 1.1 |
|---|---|
| Projects and settings | `projects`, `project_sources`, `project_config`, `templates` |
| Work | `goals`, `tasks`, `task_dependencies`, `task_labels`, `task_scopes`, `task_attachments` |
| Agents and teams | `agents`, `teams`, `team_members`, `team_tasks` |
| Execution | `runs`, `task_epochs`, `leases`, `retry_queue`, `intents`, `request_keys`, `results` |
| Raw output | `run_events`, `run_event_gaps` |
| Blobs | `blobs` |
| Messages and context | `messages`, `context_entries` |
| Housekeeping, per project | `attention` (its `project_id` is empty for a daemon-wide item), `counters`, `import_manifest` |
| Daemon-wide | `daemon_config`, `identities`, `identity_tokens`, `schema_migrations` |

| Arrives by migration in | Tables |
|---|---|
| 1.2 | `evidence`, `quarantined_hashes`, `evidence_bodies`, `uploads`, `evidence_tags`, `evidence_refs`, `search_docs`, and the FTS5 index `search` |
| 1.3 and 1.4 | `auth_failures`, `identity_projects`, `identity_certs`, `enrolment_codes` |
| 1.5 | `source_log`, `landings`, `source_push_grants`, `lease_declines`, `workers`, `worker_adapters` |

`identities` and `identity_tokens` are created now because the restore of gate line 9 must already list them.
In 1.1 `identities` holds two built-in rows, the local owner and the local worker, and `identity_tokens` is
empty outside tests.

**Columns kept for later phases** (R19), all present and unused or constant in 1.1:

- `runs`: `model`, `adapter_version`, `worker_id`, `epoch`, `run_no`, `attempt_no`, `counted`, `branch` (1.1
  writes it for a local worktree run, and R106 checks it before every use), `base_sha`, `head_sha`, `head_verified_at`, `push_window_until`, `events_pruned_at`, `events_pruned_count`,
  and the five token columns.
- `tasks`: `accepted_by`, `accepted_at`, `accepted_run_id`, `landing_state`, `landed_commit`, `landed_into`,
  `landed_at`, `landing_attempts`, `landing_conflict_paths`, `attention_reason`, `attention_since`,
  `created_by_kind`, `created_by_run_id`, `outputs`, `legacy_proof`, `position`, with the sketch's `CHECK`s.
- `goals`: `integration_branch`, `integration_head`, `published_at`, `published_commit`, `published_by`.
- `task_epochs` as a table, never deleted from and never reset.
- `project_id` in every primary key outside the daemon-wide group.
- `leases`: `worker_id`, `epoch`, `lineage`, `state`, `offered_at`, `branch`, `base_sha`, `fetch_ref`.

**Differences from the sketch**, approved with O2, which the owner answered as recommended:

| # | Difference | Reason |
|---|---|---|
| 1 | `tasks`: the check `landing_state IS NULL OR accepted_run_id IS NOT NULL` becomes `landing_state IS NULL OR landing_state = 'skipped' OR accepted_run_id IS NOT NULL`. | B13 marks every completed task `skipped`, and a 1.0 task can be done with no run (approved by hand, or its runs are gone). "Nothing to land" needs no run in any release. |
| 2 | `leases`: `branch`, `base_sha` and `fetch_ref` are nullable. | 1.1 has no hub. The table is small and short-lived, so 1.5 can rebuild it to tighten them. |
| 3 | `runs` gains `local TEXT`: a JSON object holding the pid and the workspace path. | 1.0 records both and reconcile needs the pid. They are facts of this daemon's machine, so they stay out of named columns and go when workers own processes. |
| 4 | `goals` and `messages` gain `position INTEGER NOT NULL`. | Both stores are in last-write order in 1.0 (F3). The order cannot be recovered once the index files are no longer read. |
| 5 | `runs.status` and `tasks.status` get no `CHECK`. | As in the sketch; the port's types validate them, and 1.5 adds run statuses. |
| 6 | Constants 1.1 writes into columns that only later phases vary: `leases.worker_id` and `runs.worker_id` are the built-in local worker identity; `leases.state` is `active`; `runs.counted` is 1 with `attempt_no` set; `tasks.created_by_kind` is `owner`, `agent` (a session in agent context; `created_by_run_id` set when that task holds a lease) or `daemon`, and `import` for imported tasks. | The sketch declares these `NOT NULL` or checks them, so 1.1 must say what it stores. `import` is a fourth value of `created_by_kind`: 1.0 did not record who created a task. |
| 7 | `tasks` gains a unique index over `(project_id, id COLLATE NOCASE)`. | Two task ids that differ only in the case of ASCII letters are one workspace and one branch on a filesystem that folds case (R117, F14). `NOCASE` folds the 26 ASCII letters and nothing else. The primary key stays the exact id, so every lookup is exact to the byte and no id is changed. An export holds only the automatic indexes ("Import bounds"), so it is the destination's index that judges an imported row. |
| 8 | `leases` gains `lineage TEXT NOT NULL`, and `daemon_config` holds the current lineage. | The owner's answer to AP11: a lease is fenced by the pair of lineage and epoch (R77, R132). The sketch has the epoch alone. Adding the column later would be a migration of a table every claim writes. |

Rows the daemon keeps without a table of their own: the generation, the restore lineage and the saved
dispatch state in `daemon_config`; each maintenance job's times under `maintenance/<job>` in `daemon_config`; 1.0's totals in
`counters`; `run_cap`, `preapproval`, the retention settings and `legacy.config` in `project_config`.

## Transactions and intents

| Operation | Lane | In the one transaction | Intents written with it |
|---|---|---|---|
| Start a run (R35) | Claims | Check the disk floor flag and the run cap; `claim_lease`; next epoch; insert the run as `preparing`; task to `in_progress`, attempts; delete its retry row; request key. | `process.start` |
| Finish a run (R36) | Entities | Lineage and epoch check: an earlier lineage is refused and changes no row; a stale epoch of this lineage changes this run's row only. Then run outcome, usage, model, adapter version; result row; task to `review`, `done` with `landing_state = 'skipped'`, `retrying` with a retry row, or `failed`; release the lease; agent totals and `move_to_end`; counters. | `workspace.remove` when 1.0 would remove it |
| Cancel a task (R37) | Entities | Task to `cancelled`; run to `cancelled`; release the lease; delete its retry row; request key. | `process.stop`, `workspace.remove` |
| Pause a goal (R38) | Entities | Goal to `paused`; cancel the tasks 1.0's pause cancels, each as above. | `process.stop` per cancelled run |
| Permanent failure (R39) | Entities | Task to `failed`; the cascade over its dependents and its goal, as 1.0 decides it; release the lease. | `workspace.remove` |
| Prune a run's events (R62) | Maintenance | Delete at most 1,000 events of the run and add the number to `events_pruned_count`; the last transaction sets `events_pruned_at`. | none |

| Intent kind | Performed | After a restart |
|---|---|---|
| `process.start` | Prepare the workspace as R118 says, store the prompt blob, create the agent process held, commit the run as `running` with its pid and start time in a second transaction, then release the process (R96). | Never repeated. Startup recovery handles the run as R41 says: `preparing` is ended; `running` is kept while its process lives. |
| `process.stop` | Stop the process group named in the run's `local` data, checked against its start time. | Repeated until the process is gone or was never this run's. |
| `workspace.remove` | Remove the task's workspace, once R118 has shown the directory to be that task's. | Repeated; a missing workspace is success, and so is a directory that is another name's, which is left as it is. |
| `blob.delete` | Delete a blob file after its `blobs` row is gone. | Repeated; a missing file is success. |

An intent holds ids and hashes, never a path. `process.stop` names the run, and is written only for the run of a
lease; an import brings no lease and no intent, so a pid that came from a source is never signalled (R105).
`workspace.remove` names the task; the directory is found as R106 says and removed only as R118 allows, so an
intent for one id never removes a directory that a listing shows under another. `blob.delete` holds a blob's
hash, or the project and run of an event file, and the file name is parsed or computed again (R101).

Change notifications to subscribers are not intents: they stay in memory, and a subscriber that missed one gets
the resync that protocol 2 already defines. Done intents are deleted after 7 days.

## Writer lanes

| Lane | Work | Budget for the wait |
|---|---|---|
| 1 `Claims` | Claims, renewals, releases, lease expiry | 50 ms |
| 2 `Entities` | Every entity change and the multi-entity operations above | 200 ms |
| 3 `Events` | Event batches and gap rows | 1 s |
| 4 `Maintenance` | Pruning, expiry, sweeps | 5 s |

Lane 1 always goes first; among lanes 2 to 4 the earliest deadline wins, and a bulk job queues its next
transaction only when the last has committed (R26). Lane 1 cannot starve the others: its work is bounded by
the number of tasks and agents. One transaction is bounded to 1,000 rows or 20 ms of work (R27). A collector hands over a batch when it holds
256 events or the oldest is 250 ms old. Its buffer holds at most 4,096 events or 4 MiB per run; past that it
drops the oldest raw output and records a gap. Batches that wait are committed together: the event lane has
one transaction queued for all runs, up to R27's bound, so an entity change waits behind at most one event
transaction however many runs produce output (R26). Results and usage travel on lane 2; a finish is spooled
to disk before it is queued (R129), so neither a failed writer nor a restart loses it.

**What the lanes do not bound.** The lanes fix an order. The waits in the table are budgets, and only a
measurement shows whether a platform meets them (R5, R63). R27's 20 ms is checked between statements: a
statement that is running, and a commit, are not cut short, and the first measurement saw both take hundreds
of milliseconds on macOS (AP13). No requirement here derives a time from the order. Since the owner's
decision of 2026-11-21 no such measurement is part of 1.1 (record I of "Owner's answers"): the waits stay
as written, as the deadlines that order lanes 2 to 4 and as figures that the writer's metrics show, and
nothing in 1.1 checks a platform against them.

**What each number of the table is.** Lanes 2 to 4 use theirs as deadlines: the writer orders their
transactions by arrival plus the lane's wait (R26). Lane 1's 50 ms orders nothing, because a claim always
goes first; it is the target for the time a claim waits before its transaction starts, which
`writer_wait_seconds` shows. None of the four is a gate. The one gate on a time is gate line 7, read
against the claim-latency budget below, which runs from a claim's arrival to its commit and so holds the
wait, the claim's own transaction and its commit. "Event measurement" says how the proposed budget sits
beside the 50 ms, the 1 s and the collector's 256 events or 250 ms. Since 2026-11-21 gate line 7 is retired
and 1.1 has no gate on a time (record I of "Owner's answers").

**Checkpoints** (R126, R127). The writer never checkpoints in a commit: automatic checkpointing is off on
its connection. The `wal.checkpoint` job runs passive checkpoints on a connection of its own, which SQLite
allows beside a writer, 5 minutes after its last completed one or when the backlog reaches 64 MiB. A
completed one is any call that succeeded, whichever rule started it and whatever it copied; R137 names the
outcomes of a call and says which of them move the timer.

- **The backlog is not the file.** The backlog is the frames in the log that no checkpoint has yet copied
  into the database, times the page size. The `-wal` file is something else: SQLite uses it again from the
  start after a complete checkpoint and does not shorten it, so after one long backlog the file stays long
  while the backlog is zero. Every rule here acts on the backlog. A rule on the file's length would stay
  tripped until the daemon's last connection closed, and "run it again" would fail the same way each time.
  `orch db status` shows both numbers.
- **Where the backlog comes from.** The writer's commit callback gives the frames in the log after each
  commit, and a checkpoint's result gives the frames in the log and the frames copied. The result is exact
  and resets the count; the callback advances it between checkpoints. Setting `wal_autocheckpoint`
  replaces a connection's commit callback, so the writer sets it to 0 first and installs its callback
  after. The library has both calls: `wal_hook` under its feature `hooks`, and
  `wal_checkpoint_v2` under `wal`, which `hooks` turns on (ADR 0017 decision 19). Both were read in the
  library's source. The library report's probe does not run them, so the harness of "Event measurement"
  is the first to run them and R126's tests are their proof. Since 2026-11-21 that harness is deferred
  (record I of "Owner's answers"), so R126's tests are the first to run them as well.
- **Long reads.** A read that stays open keeps frames from being copied, so over 256 MiB of backlog a
  pooled read older than 30 s is interrupted (`snapshot_expired`): a backup or an export that this stops
  is run again, and succeeds once the backlog has fallen. A reader outside the daemon, such as a `sqlite3`
  shell left inside a transaction, cannot be interrupted; the RUNBOOK names the cause.
- **What is synced, and when.** With the write-ahead log and `synchronous = NORMAL` (B12), a commit is not
  synced: the log is synced before a checkpoint, and the database file when the checkpoint has copied its
  frames. The database stays consistent across a power loss, and the commits since the last sync can be
  lost; `FULL` syncs the log at every commit and is the setting for an owner who wants otherwise. On macOS an ordinary `fsync` does not
  ask the drive to empty its cache; `fullfsync = ON` makes SQLite use `F_FULLFSYNC` for every sync, and
  `checkpoint_fullfsync = ON` says the same for checkpoints alone, set as well so that the intent is on
  record. The proposal is both on, on every connection that can write: a checkpoint then costs what a real
  flush costs, and "Event measurement" measures it. On Linux the two settings are set and read back and
  change nothing: syncs are the platform's `fsync`, and setting them is no evidence about a drive's cache.
  Neither platform's settings are a power-loss test. Since 2026-11-21 the measurement that was to show
  their cost is deferred, and so is the owner's ruling on the proposal (record I of "Owner's answers"); in 1.1
  both settings are built as R126 states them, and R126's tests read them back.

**When a transaction fails** (R128). Failures that come from outside the transaction's own logic:

| Class | SQLite results | The writer |
|---|---|---|
| Retryable | `SQLITE_BUSY` or `SQLITE_LOCKED` past the busy timeout, `SQLITE_FULL`, a first `SQLITE_IOERR` | Rolls back and tries the same transaction again after 50 ms, doubling to 2 s. Nothing is dropped. No claim is granted meanwhile. A request whose deadline passes is answered `overloaded`, withdrawn, and never committed later; the daemon's own work (finishes, lease changes, events) stays queued |
| Fatal | `SQLITE_CORRUPT`, `SQLITE_NOTADB`, a second `SQLITE_IOERR` in a row, one transaction retried for 60 s without committing (an idle writer has nothing to retry and never fails for that) | Fails: every mutation is refused with `storage_unavailable`, no claim is granted, runs stay supervised and their finishes are spooled, and `daemon.status`, `orch status` and `orch doctor` report it from memory. A restart is the only way out |

The times are proposals. A logic error (a stale revision, a constraint) is the request's own answer and is
not in this table.

**When another connection fails** (R140). The writer is not the only connection on the live database: the
pool reads, the checkpoint job has a connection of its own, `orch db query` has one (R130), and a backup or
an export reads through one.

| Seen on | Result | What follows |
|---|---|---|
| Any of them | `SQLITE_CORRUPT`, `SQLITE_NOTADB` | The writer fails at once, as in the fatal class above, and the report names the connection and the result. No checkpoint is called afterwards |
| The checkpoint connection | A call ends `busy` or `failed` (R137) | No timer moves, and the call is tried again at a later look. Three in a row are reported until a call succeeds |
| The checkpoint connection | Calls succeed, and the backlog stays over 256 MiB for more than 5 minutes | Reported with the backlog, the time, the age of the oldest read and the last call's outcome, until the backlog is under the limit |
| A pooled read | Any other failure | The request gets the error and the connection is opened afresh. Three in a row across the pool are reported until a read succeeds |
| A backup, an export, `orch db query` | Any other failure | The command fails and says why, and the last such failure is shown |

The reports are made from memory by `daemon.status`, `orch status` and `orch doctor` (exit 1), as the failed
writer's is. A corrupt backup, export or import file is not the live database and is that command's own
refusal. The threes, the 5 minutes and the use of R127's 256 MiB here are proposals with no measurement
behind them. None of these is an attention item: 1.1 has two kinds and adds none (R81).

**Finish spool** (R129). `spool/finish/<name>` holds one finish until its transaction commits. The name is
the lower-case hex SHA-256 of `orch-finish-file-v1`, a zero byte, and the project id and the run id each
preceded by its length as eight big-endian bytes ("Filesystem names", whose run-id vectors are proved on
this file's write, recovery and removal); for project `prj_test1` and run `run_test1` it is
`7a402d9d06dbc9a9c1e288e8ee7d2636bdab972350a11cb513de22ca059a8ae6`. The file holds the lineage and epoch
of the run's lease, and recovery applies it by R36's rule, so a file of an earlier lineage changes nothing.

Two losses remain, and both are stated. A finish whose process ended while no daemon ran is lost, and R41
ends that run as 1.0 does. And the spool write can itself fail, most likely on a full disk, when the writer
is retrying for the same reason: the daemon then keeps the finish in memory, keeps trying both the file and
the commit, grants no claim, and reports the count; if the owner stops the daemon before either succeeds,
the stop names the run and that result is lost.

**Stopping** (R139). A stop is ordered and bounded. Admission closes first; the transaction in progress
ends; the writer takes what is queued until the drain's deadline; and what is still uncommitted then is
left in a form that the next start can use: a finish in its spool (R129), and for events the range of
their numbers in `spool/events-at-stop`, which the next start turns into a gap row (R61). Reads are then
interrupted, a checkpoint call in progress is left to return, and the lock goes last. The proposed times
are 10 s for the drain, 5 s more for the reads and the checkpoint call, and 20 s for the whole stop; none
is measured. The stop ends at its bound whatever is still running, which is safe because committed work
is in the log and the next start reads the log again. One more loss is stated: events that are neither
committed nor recorded, because the file's write failed as well, are named by run in the stop's report.
R139 covers a stop that was asked for. What the collectors of a killed daemon held is outside it and is
not changed here.

*The record of a stop* (R61, R139). Events get their numbers at handover, so what was handed over and not
committed has a first and a last number for each run. The ranges are taken once the writer has ended its
last transaction and takes no other, and after that no event is given a number, so no commit lands
inside a range that was already taken. A start does not rest on that: before it writes a row it cuts
each recorded range to the numbers that no committed event and no gap row holds, so a gap row never
covers an event that exists. The record is never written in place: each version
goes to a temporary file in `spool/`, is synced, and is renamed onto `spool/events-at-stop`, and the
directory is synced, so a stop whose process is killed leaves the earlier version or the new one and
never a part of one. A temporary file left behind is removed, unread, by the next start. A stop that finds a record
already there merges it: the new version holds the earlier ranges and its own. The next start reads the
record before it gives any number and before admission opens, sets each run's counter above everything
committed and everything recorded, writes the gap rows in one transaction, and removes the record only
when that transaction can no longer be lost with the power: after a commit at `synchronous = FULL`, or
after a later checkpoint call that ended `copied_all`. Which of the two the daemon uses is the builder's
choice in T15 and T16; the tests assert the order. This is the rule that the red team's review of
`196f9087` asks, in its item O8, for a finish file. That item stays open: nothing here changes R129. It
is not this spec's open point O8, the expiry of request keys, which the owner decided.

| What happens | What must hold |
|---|---|
| A start is killed after the gap rows commit and before the record is removed | The next start finds the record, writes no second row for a range a gap row covers, and removes the record after the durable point |
| A start is refused before it reads the record (a migration fails, the marker of a restore stands) | The record is unchanged |
| Power is lost after a `NORMAL` commit of the gap rows and before the durable point | The record is still there; the next start writes the rows again |
| A stop occurs while an earlier record has not been removed yet | The new record holds the earlier ranges and the new ones; no committed or recorded number is given again (R61) |
| A stop is killed between writing its temporary file and the rename | The earlier record stands whole; the events of this stop are a loss that no record names, as for a killed daemon |
| The record is of another lineage or fails its check | Removed and counted; no row is written from it |
| A recorded range holds numbers that are committed as events (the range was taken too early, or the record was not written by this program) | The start writes rows only for the numbers that no committed event and no gap row holds; no row covers a committed event |
| The daemon is killed, or power is lost under `synchronous = NORMAL` after commits that were not yet synced | The numbers of what was lost are in no record and in no row. The counter continues above what survived, so those numbers may be given again to later events, and nothing shows it (R61). A reader that kept such a number from before the loss can find another event under it afterwards |
| Power is lost while a record is published or soon after, and a sync had not reached the medium | The record may be absent, be the earlier version, or fail its check. In the first two cases the ranges of this stop are lost as for a killed daemon; in the third the record is removed and counted, and the count is all that is left of its ranges ("What a sync promises") |

Where a service manager runs the daemon, the time it allows a stopping process before it kills it must be
longer than the whole stop's bound. The RUNBOOK is to say so where it describes running the daemon under
one; that is a duty of the documentation task and is not written yet. No service manager's default is
stated here, because none was checked.

**What a sync promises** (R129, R138, R139). The marker of a restore, the record of a stop, a finish
file, their temporary files, and every directory that holds one of them or that a rename of a restore
touches are written and synced by this program and not by SQLite. R126's two settings act on SQLite's
own syncs and do not reach them. This spec promises two different things for these files, and says here
which is which.

*The process stops and the operating system keeps running*: a signal, a kill, the out-of-memory killer,
a crash of the program. No promise for this case rests on a sync. A file that is created exclusively,
written and renamed onto its name is seen whole or not at all by the next process, and each rename of a
member is seen done or not done. Wherever R138, R139 and the text under them say what "a stop" or "a
kill" leaves, they mean this case, and this is the case that their kill tests prove.

*Power is lost, or the operating system itself stops.* What survives is what the platform's sync had put
on the medium, and nothing more is promised. "Synced" means, for these files and directories:

| Platform | A file | A directory | What is known |
|---|---|---|---|
| Linux | `fsync` | `fsync` | It is the platform's call. Nothing here is evidence about a drive's cache, as "What is synced, and when" says of SQLite's own syncs |
| macOS | `F_FULLFSYNC` | `F_FULLFSYNC` where the system accepts it for a directory, and `fsync` where it refuses it | The system's manual says that `fsync` does not ask the drive to empty its cache, and that `F_FULLFSYNC` syncs as `fsync` does and then asks the drive to flush. That was read in the manual and was not tested. Whether the call is accepted for a directory was not checked |

This carries the intent of R126 to the files that SQLite does not write, and like R126's sync settings
it is a proposal. It is not a proof: no power-loss test was run, and none is among the tests of 1.1. The
PRs that build these files (T15, T16 and T23) state which call each platform made for a file and for a
directory, and correct this table where the system answers otherwise. Since the owner's ruling of 2026-11-23 (record K) those PRs state and test the Linux row only: the macOS row is kept as written, stays untested, and whether macOS accepts the call for a directory is a macOS-only check deferred to the [After 2.0](https://github.com/yuxhuang/r-orch/milestone/20) milestone. A sync that fails stops the step
that asked for it, as a failed rename does.

After a loss of power, where a sync had not reached the medium, these outcomes are possible. Each is
stated here and none is ruled out:

| What the next start finds | What follows |
|---|---|
| A restore's marker that fails its check, because its name survived and its bytes did not | It fails closed: nothing starts, both commands refuse, and the owner recovers by hand as the RUNBOOK is to say (T26). That publishing a marker "cannot leave such a file" is true of a stop of the process only |
| A temporary marker file, a staging directory or an empty `pre-restore` directory, and no marker | Scratch: removed unread, as after a stop in step 2 or step 3 of a restore |
| Members that a restore had moved, and no marker, because the marker's own rename did not survive and later renames did | Not recognised. No check of a start tells this state from an ordinary one, and this spec adds none: a start is not refused. It is excluded only as far as the platform keeps on the medium what its syncs reported, in the order in which they reported it. The way back is by hand, from the `pre-restore` directory or from the backup |
| A stop's record that is absent, or that is the earlier version | The ranges of that stop are lost with no record, as for a daemon that was killed |
| A stop's record that fails its check | Removed and counted (R139). Its ranges are lost, and the count is all that is left of them |
| A stop's record that is still there after its gap rows were written | No second row is written for a range that a row already covers (R139) |

A finish file is synced in the same sense. When it may be removed is item O8 of the red team's review of
`196f9087` (not this spec's open point O8), which stays open, and nothing here changes R129.

**Deferred by the owner on 2026-11-21** (record I of "Owner's answers"). The two parts that follow,
"Claim-latency budget" and "Event fallback", are kept as a record and are not in force in release 1.1. There
is no claim-latency budget to meet in 1.1 and no measurement against one. The event fallback is not built, and
no file, table or setting of it exists in 1.1. Events are stored in SQLite by the batched design above (R63).
The first run's figures and its FAIL are kept ([issue 98](https://github.com/yuxhuang/r-orch/issues/98)).

**Claim-latency budget** (deferred on 2026-11-21; deliverable D, gate line 7, AP13). There are two budgets,
and neither replaces the other.

*The first budget and the first run.* The measurement declared it before it ran
(`docs/sqlite-event-throughput.md`). With 1, 6 and 24 runs producing events continuously, at the nominal
rate and at ten times it, 50 claims a second, and `synchronous = NORMAL`:

- a claim, from its scheduled arrival to its commit, queue time included, takes at most 50 ms at the 99th
  percentile and at most 100 ms at the worst;
- no event batch waits more than 1 s, and every generated event is committed.

The nominal rate is one event a second for each run, sized from the aggregate figures of S2; the report states
its assumptions beside its figures. The first run missed this budget on macOS and met it in a Linux container
on the same machine (AP13 has the figures). That verdict stands. The run's budget, samples, figures and FAIL
are kept as they are, and no later run is pooled with them or reported in their place.

*The revised budget.* On 2026-11-20 the owner chose to revise the workload and the budget and to measure
anew, and set no numbers; the answers of 2026-11-21, which settled every other point, kept this one as it
stood. "Event measurement" is the proposal: a workload nearer to real load, 200 ms at the
99th percentile and 1,000 ms at the worst, and a method in which every repetition contains the production
checkpoint. Its numbers are proposed and not accepted. The proposal and the results of its run go back to
the owner together, and until the owner has accepted both, gate line 7 is unproven. Every miss is kept. A
miss selects the event fallback only when it is attributable to events, by the owner's ruling of
2026-11-21; a miss without events, or on one platform alone, selects no event path and goes back to the
owner ("Event measurement"). A fallback that is selected is then measured the same way, and whichever
path ships is measured as built before its PR merges (R63). A green test suite is not a performance result.

**Event fallback** (not built in 1.1, by the decision of 2026-11-21; R63, AP13). The goal names it:
append-only files behind the blob-store adapter, with one index row for each batch. The port does not change.

- A batch is appended to the run's file `blobs/events/ab/<event file name>`, and then one row of
  `run_event_batches` (run, `first_seq`, `last_seq`, `offset`, `length`, `sha256`) is committed on lane 3. The
  name is 64 hex digits that the daemon computes from the project id and the run id, and `ab` is its first two
  ("Filesystem names", R101): the run id itself is never part of a path. The file is created exclusively and
  reached as R102 says. Bytes come before the row, as for blobs: a stop leaves at worst an unindexed tail,
  which the next append to that run cuts off.
- The file is not synced for each batch. After a power loss a batch whose bytes are missing or do not hash
  to its row is reported as a gap; results and usage are rows and are not affected.
- `append_events`, `read_events`, `read_events_tail` and `prune_events` keep their contracts: a repeated
  batch is recognised by its sequence numbers and stores nothing, reads go through the index rows, and
  pruning deletes the rows and then the file by a `blob.delete` intent. Each of them finds the file by
  computing its name again.
- 1.1 then creates `run_event_batches` in place of `run_events`, still 33 tables. A backup copies each event
  file, under the same name, up to the length its rows cover, and an export carries the indexed bytes in its
  blob table, not as files.
- No figure yet shows that the fallback meets either budget (AP13). If it ships it is measured as R63
  says, and nothing here assumes that it is faster.

## Event measurement

**Deferred by the owner on 2026-11-21** (record I of "Owner's answers"). This whole section is kept as a
record, and none of it is in force in release 1.1: the method `ap13-realistic-v1`, the proposed budget, the
cells, the verdict, "What a miss selects", the host and platform rules, and every proposal among them. No data
set is authorised and none is run. `protocol.json` is not written, and no harness or verifier is built for
this section. Nothing in 1.1 waits for anything here. The owner's ruling and the answers that this section
carries (records E, F and H) are not withdrawn: they have nothing to act on until latency is taken up again.
What this section names as open or as a proposal stays so, not settled and not due. The review findings that
exist only for this measurement are parked with it and are not closed. The first run's figures are in
[issue 98](https://github.com/yuxhuang/r-orch/issues/98). Below this note the section is unchanged.

A second note, added on 2026-11-23 (record K of "Owner's answers"). The measurement stays deferred by record I, and
record K does not retire it again or answer any of its open points. It adds only that the macOS hardware class
and the macOS block of this section are outside the platform scope of every release through 2.0, so that were
latency taken up again, the measurement would be planned for Linux. The figures of the first run, which include its
miss on macOS, are prior evidence: they are kept as recorded, with the platform they were taken on, in
[issue 98](https://github.com/yuxhuang/r-orch/issues/98), and no text here turns them into a requirement.

The first run stands as measured ("Writer lanes"). This section states the second measurement before it
runs, as the owner directed (AP13): method `ap13-realistic-v1`, proposed by the performance engineer on
2026-11-20. Every number in it is proposed and none is accepted. Its machine-readable form,
`docs/experiments/sqlite-events/protocol.json`, is written from this section, and its SHA-256 is recorded
with this spec's commit before the first repetition. Where the two differ this section rules, and the run
does not start. What this section leaves to the file (the bounds of the harness's queues, 512 claims and
eight event batches; sampling periods; the layout of results) is fixed there before the run and listed in
the report, and may change no limit, cell or rule stated here. A protocol file written from an earlier
revision of this section does not stand for this one: a file written from this text is a new file with a
new digest, and it is challenged before a repetition runs.

**What the numbers rest on.** The measured inputs are the aggregates of S2 (about 700 MB of events for
2,538 runs) and the first run's report. They hold no interval, no rate, no concurrency and no peak. The
workload is therefore made of assumptions, each named as one: 1, 6 or 24 runs producing events at once; one
event a second for each; one claim a second; and the burst of cell B24. One claim a second is not a measured
peak. It is 12.5 times the 0.08 starts a second that 24 runs of five minutes each would need, and that model
is an assumption too. The first run remains the only evidence at 50 claims a second. Retries, finishes,
renewals and the socket are not in this experiment; they are in R63's measurement of the path that ships.

**Proposed budget.** In every mandatory cell, on each platform, in every repetition taken alone:

- a claim, from its scheduled arrival to its commit, with queueing, a late producer and blocked admission
  counted, takes at most 200 ms at the 99th percentile and at most 1,000 ms at the worst;
- the same two limits hold for the claims around each checkpoint and for the claims of each burst;
- no event is older than 5,000 ms from its arrival to its commit; no claim and no event is lost or stored
  twice; nothing commits after it was refused; every queue is empty within 10 s of the last arrival; there
  is no unexpected error, no corruption, and no resource limit is reached.

No pooling across repetitions, cells or platforms rescues a miss. The 200 ms and the 1,000 ms are a
judgement of how long background agent work may wait to start, made before any result exists; no study of
users and no measurement backs them. The limit on an event's age is there so that claims cannot pass by
starving events.

**Cells.** Five repetitions of each, each on a fresh database in a fresh process, with 660 s of arrivals.
Nothing is discarded as warm-up, and the slow claims at the start and the end are kept.

| Cell | Event streams | Claims | Stored history | What it is for |
|---|---|---|---|---|
| C0 | none | 1 a second | 668 MiB | Claims alone, under the same checkpoint rule |
| C1 | 1, at 1 event a second | 1 a second | 668 MiB | A small workload |
| C6 | 6, each at 1 event a second | 1 a second | 668 MiB | The assumed usual load |
| C24 | 24, each at 1 event a second | 1 a second | 668 MiB | The assumed reserve of concurrency |
| B24 | 24 aligned: 10 events a second each for the first 5 s of every 60 s, then 1 a second | 1 a second, and 24 more at once at 0, 60, ... 600 s | 668 MiB | An assumed burst beyond the usual load: 24 claims at once, four times the six runs of C6. Two bursts fall due at 300 s and 600 s, where timer checkpoints are expected; whether a burst meets a checkpoint is counted and never arranged |
| H32 | as C24 | 1 a second | 32 MiB | Whether the size of the history matters |
| K64 | as C24 | 1 a second | 668 MiB, and a backlog of 64 to 80 MiB at the start | The checkpoint that the size rule starts |

A stream repeats four records of 128 bytes and one of 4,096 bytes, the first run's mix. Streams start
1/N s apart, except in B24, where all are aligned. Claims are due at k + phase seconds for k = 0 to 659,
with a phase of 0.1, 0.3, 0.5, 0.7 and 0.9 s in repetitions 1 to 5: 660 claims in a repetition, and 924 in
B24. Each claim takes a task made beforehand and, in one `BEGIN IMMEDIATE` transaction, inserts a lease and
a run and updates the task, as in the first run. Events are batched over 50 ms for all streams together,
with one event transaction queued at a time, cut at 1,000 rows or 20 ms, and a claim taken first between
transactions. The history is synthetic event payload over 2,538 run keys, seeded as in the first run; the
sizes of the database and the log that result are recorded and not promised. Cells run one after another,
C0 to K64 in odd repetitions and K64 to C0 in even ones, never two at once and never two platforms at once.

**Checkpoints in every repetition.** This replaces the first run's 12 s cells. Those ran with SQLite's
automatic checkpointing on (`wal_autocheckpoint = 1000`), so a checkpoint in them ran inside a commit of
the writer. That is not the rule of this spec (R126), and no checkpoint of R126's job could occur in them.
A 12 s cell with automatic checkpointing off, as the diagnosis first proposed, would have held no
checkpoint at all (red team W2-R1).

- The connections are set as R126 says and as B12 says: the writer with automatic checkpointing off, the
  checkpoint job on a connection of its own, passive, 300 s after its last completed checkpoint or at 64 MiB
  of backlog, looked at once a second. The timer starts with the arrivals. Each connection's settings are
  read back and recorded, with the SQLite version and its compile options.
- In every cell two timer checkpoints are expected, near 300 s and near 600 s. A timer checkpoint that
  counts is a call that the timer rule started (R137), that started after the arrivals began and returned
  before 660 s with traffic running, and whose outcome is `copied_all` or `copied_part`. A call that the
  size rule started, a call at shutdown or after the seeding, and a call that ended `nothing_to_copy`,
  `held_back`, `busy`, `failed` or `unknown` never count as one. The trigger is not shortened, no call is
  made or moved to obtain coverage, and no repetition is extended.
- A repetition without two such calls has **incomplete coverage**. That is a result of its own, reported
  apart, with its reason and with every checkpoint call of the repetition (what started it, its start and
  end, its outcome and both counts). It is never a pass, never a miss of the budget and no evidence about
  events. The claims and events of that repetition are judged all the same, and a miss among them stays a
  miss ("The verdict").
- The production rule can itself leave a repetition without coverage, and such a run is a valid run of
  the rule and no fault of the harness. Every completed call moves the timer (R137). If the size rule
  starts calls that complete at 250 s and at 500 s, the timer is next due at 800 s, after the arrivals
  end, and the repetition holds no timer checkpoint at all. In K64 the call at the start moves the timer
  by its own length. And in every cell the second timer call is due 300 s after the first one ends and is
  seen at the next look, so both fit before 660 s only while the two calls and the two looks take under
  60 s together. These are consequences of the rule, worked out and not measured. Such a repetition is
  reported as incomplete coverage with one of the reasons `timer_moved_by_size_call`,
  `timer_call_copied_nothing`, `timer_call_busy_or_failed` and `timer_call_after_arrivals`, and its cell
  is incomplete. Whether the coverage rule should change is a question for a new revision of this method,
  reviewed before it runs; the harness does not answer it, and this revision does not change the rule.
- Which cells this reaches is not known. The size rule starts a call when 64 MiB of the log arrive within
  300 s, about 224 kB a second. The mandatory cells that hold events, C24, H32 and B24, may come near
  that rate or pass it, and so may the diagnostic at 10 events a second; K64 meets the size rule by its
  design. No figure of the log's growth under R126's settings exists for any of them: the first run
  ran with SQLite's automatic checkpointing and not with these settings. If the size rule comes first in every
  repetition of a mandatory cell, the data set can never be complete, and it then selects no path
  whatever SQLite does ("What a miss selects"). Whether the method can complete is therefore open. It is
  the performance engineer's to settle in a reviewed revision of the method before the cells are frozen;
  this revision forces no call, credits no other call, lengthens no repetition and measures nothing to
  find out.
- K64 also begins with a backlog of 64 to 80 MiB, made before the arrivals start and kept apart from the
  measured counts. The size rule must start a checkpoint at once, on its own connection, while the traffic
  of C24 begins, and that checkpoint must copy at least 64 MiB of frames.
- The sync settings are those of "Writer lanes": `fullfsync` and `checkpoint_fullfsync` on, on both
  connections, on both platforms, read back; `synchronous` stays `NORMAL`. On macOS that is the proposed
  product setting and makes every sync a full flush. On Linux it records the values and claims nothing
  about a drive's cache. A checkpoint's time is recorded apart from the writer's begin, statements and
  commit and from a claim's time in the queue.
- **The window around a checkpoint**, fixed now: from 30 s before its start to 30 s after its end, cut to
  the measured interval. Its claims are those due inside it and those whose time from due to commit crosses
  the checkpoint. Each window is reported and judged alone, and so is their union in a repetition; the
  claims that cross the checkpoint itself and the claims outside every window are reported beside them. A
  window of fewer than 100 claims has its 99th percentile at its worst claim, so there the 200 ms limit
  judges the worst claim. That is stricter on purpose, the owner should know it, and it is not loosened
  after results exist. A checkpoint too short for any claim to cross it has none that cross, and the
  report says so. A burst's window is the 10 s from its due time.
- **What is reported about overlap.** For each checkpoint call of a repetition the report gives how many
  claims have a time from due to commit that crosses the call, and in B24 how many of those are claims of
  a burst. A count of zero is a result and is written as zero, with the sentence that no claim met that
  checkpoint. Nothing is arranged to make the two meet: the bursts are due on the minute, the timer runs
  from the last completed call, and a burst at 300 s or 600 s may be committed before the checkpoint
  starts. A B24 repetition that passes with a count of zero says nothing about a burst during a checkpoint.

**Statistics.** Each claim is kept with its due, queued, started and committed times. Percentiles are by
nearest rank and never averaged. The report gives every repetition; the least, the middle and the greatest of
a cell's 99th percentiles; the pooled raw samples as a second view; and, for each population, how many claims
exceeded 50, 100, 200 and 1,000 ms, so that the owner sees what the first budget's limits would have ruled on
the new workload. Two counts are reported beside them, from samples the method already keeps, and decide
nothing: the claims whose wait before their transaction started exceeded 50 ms, with that wait given apart
from the transaction and its commit; and the events older than 1,000 ms at their commit. An interval for the
99th percentile is given by a moving-block bootstrap (blocks of 30 s within a repetition, 10,000 resamples,
seed 1301) and is descriptive: it excuses no observed miss. Five repetitions that pass are evidence for those
runs and conditions, not a bound on any other.

**The verdict.** One rule gives every data set its verdict. It is applied to each repetition, then to each
cell, to each platform and to both platforms together, and it reports four axes side by side. No axis hides
another, and nothing that was observed is dropped to make an axis look better.

| Axis | Values | What decides it |
|---|---|---|
| Correctness | `ok`, `defect`, `unknown` | `defect`: an outcome that is known and wrong: an event lost or stored twice, a wrong claim outcome, a commit after a refusal, corruption, an unexpected error from SQLite, growth without bound. `unknown`: the counts or the integrity result that would show it are absent |
| Validity | `valid`, `host_not_idle`, `invalid` | `host_not_idle`: a measurement of the declared backend on a host that did not meet "The host". `invalid`: not a measurement of the declared thing at all: the wrong backend or one that is not shown, settings that do not read back, a failed timing check, a platform outside its class |
| Coverage | `complete`, `incomplete` | `incomplete`: something that must exist is absent or does not count: a cell, a repetition, a required platform, a diagnostic, a timer checkpoint that counts (incomplete coverage), K64's size checkpoint, the host record, a receipt, or a run that a resource limit stopped or that has no end record |
| Budget | `met`, `missed`, `not judged` | `missed`: something observed exceeded a limit of the proposed budget: a claim, a window, a burst, an event's age, or a claim or an event that was refused or was still queued 10 s after the last arrival. `not judged`: the repetition is `invalid`, or nothing was observed |

A recorded outcome that is wrong and a record that is absent are different things and are never exchanged.
A scheduled claim with a recorded refusal, or one recorded as still queued at the drain's deadline, is an
observed outcome and a miss of the budget; it is not taken out of the sample and it is not "missing". A
claim or an event with no record, in a run that was cut short, is absent: the coverage is incomplete and
nothing is concluded about it. Counts that disagree with the schedule in a run that ended normally are a
defect. On a host that was not idle a miss is still `missed`, and a budget that was met is `met` and does
not count toward a pass. What an `invalid` repetition showed over a limit is listed as observed on invalid
evidence; it is neither a miss nor a pass. Two consequences are stated so that no case has two answers.
Correctness `unknown` always comes with incomplete coverage, because what would show the correctness is a
record that must exist. And an `invalid` repetition does not count, so its cell's coverage is incomplete
until the set of five has been run again for it, as below.

The headline is the first of these that applies, and every axis is printed with it:

1. `CORRECTNESS_DEFECT`: a repetition, mandatory or diagnostic, has a defect. It goes to the builder before
   any performance result is used.
2. `FAIL`: a mandatory repetition that is `valid` or `host_not_idle` has `missed`. When the coverage is
   also incomplete the headline is `FAIL, INCOMPLETE`, and both stand: the miss is a failure that no later
   data removes, and the data set is not complete.
3. `INCOMPLETE`: no defect and no miss, and something is `unknown`, `host_not_idle`, `invalid`,
   `incomplete` or `not judged`. A budget that was met on a host that was not idle ends here.
4. `PASS`: every mandatory repetition of every cell on both required platforms is `ok`, `valid`,
   `complete` and `met`, and every diagnostic is present with its coverage and without a defect.

The program that computes the verdict exits 0 for `PASS`, 1 for `FAIL` with or without `INCOMPLETE`, 2 for
`INCOMPLETE` and 3 for `CORRECTNESS_DEFECT`; any other exit is that program's own failure and is no
verdict. Its machine-readable result carries the four axes for every repetition, cell and platform and for
both platforms together, so an exit of 1 never hides what is incomplete and an exit of 3 never hides a
miss. A diagnostic's latency decides nothing on the budget axis; a diagnostic that is absent or lacks its
coverage makes the coverage incomplete.

What is incomplete is kept. The defect is fixed in a new data set that names the old one, and nothing is
run again until it passes. A repetition is replaced only when its measurement was `invalid` or
`host_not_idle` for a reason of this section, written before the run; the replaced one is kept with
everything it showed, and the whole set of five is run again. A miss is never replaced: it stays in every
report beside any later data set. After a data set in which a mandatory repetition that is `valid` or
`host_not_idle` has `missed`, no further data set is started without the owner's word, whatever the
headline is: `FAIL`, `FAIL, INCOMPLETE` or `CORRECTNESS_DEFECT`. A data set whose only purpose is to fix
a defect is a further data set, and there is no exception for it. The rule reads the budget axis, which
is printed with every headline, and not the headline word; where it applies, neither the sentence on a
defect above nor the rule for an `INCOMPLETE` data set below starts a data set in its place. That is the
owner's answer of 2026-11-21, "ask first" (record F of "Owner's answers"); it permits no measurement.
An `INCOMPLETE` data set selects no path and is no reason of its own to measure again. After one, a
new data set is started only on the owner's explicit direction, or under a revision of this method that
was reviewed and approved before the new run and that names the old data set. The unchanged method is
never run again to see whether another run happens to complete, and incomplete checkpoint coverage is
not one of the reasons for which a repetition is replaced.

Two questions about that rule were not settled by the owner's words "ask first", and the text of
`8954cf6` settled neither. Both were answered on 2026-11-21 under the owner's delegation (record H of "Owner's
answers"). The first: when the first platform's block has a mandatory miss in a repetition that is `valid`
or `host_not_idle`, starting the second platform's block is further measurement and needs the owner's word.
Until that word is given the data set stands as it is, with a platform absent: its coverage is `incomplete`
and its budget axis is `missed`, it selects no path, nothing is put in the place of the block that was not
run, and nothing is said from it about what both platforms show. Its headline and its exit are the ones
that the rule above gives, and no others: `FAIL, INCOMPLETE` and 1 when no repetition has a defect, and
`CORRECTNESS_DEFECT` and 3, with those two axes printed, when a repetition, mandatory or diagnostic, has
one. The second: while a data set holds a mandatory miss in a
repetition that is `valid` or `host_not_idle`, replacing an `invalid` or `host_not_idle` set of five is
further measurement and needs the owner's word. The old set and every miss are kept, and a replacement that
the owner allows removes neither. The second answer reads a mandatory miss that already stands. A set of
five that is `invalid` or `host_not_idle` in a data set with no such miss is replaced as the paragraph above
says, and a diagnostic's latency is no miss.

Neither answer permits a measurement, and neither is an exception to "ask first": each says that a thing is
covered by it. Neither changes a headline or an exit. Both read the budget axis, as "ask first" does, and
the headline stays the first that applies of the four above: a defect comes first whatever is missed, absent
or `invalid` beside it, and the other axes are printed with it.

Two things are settled by no answer and are open (AP13 of "Open questions"). The first is what is done
with a repetition or a block that is already running when a mandatory miss is seen. The second is whether
the blocks of the two platforms may run at the same time or in turns, so that one is already started when
the other shows its miss: the first answer is about starting a block, and it says nothing of a block that is
started. This text chooses no rule for stopping, scheduling or tearing down a block, and no measurement
campaign is started before the two are settled. Whatever a block has produced is kept, as everything
incomplete is. A third thing is open as well: whether a block that the owner allows after a miss completes
the data set that holds the miss or is a data set of its own. No answer says which, and this text chooses
neither.

What may follow a data set, as that rule and the two answers must give it. The rows were traced against
this text by reading; nothing was run for them, and the 1,200 ms is the value of the case. No repetition
has a defect in a row that names none.

| Data set | Headline and exit | A further data set |
|---|---|---|
| A diagnostic repetition stores an event twice; C24 has a claim at 1,200 ms in valid repetitions on both platforms; all else met, complete and idle. A fix for the duplication is ready, and a new data set would change nothing else | `CORRECTNESS_DEFECT`, 3, with the budget axis `missed` printed | Only on the owner's word, although it only fixes the defect: a mandatory `valid` repetition missed |
| A diagnostic repetition stores an event twice; one mandatory C24 repetition on macOS has a claim at 1,200 ms on a host that was not idle; all else met | `CORRECTNESS_DEFECT`, 3, with `missed` printed | Only on the owner's word: a mandatory `host_not_idle` repetition missed |
| A diagnostic repetition stores an event twice; no mandatory repetition is over a limit | `CORRECTNESS_DEFECT`, 3 | The defect is fixed in a new data set that names the old one; this rule asks for nothing, since no mandatory repetition missed |
| The macOS block is complete, and one mandatory C24 repetition in it, `valid`, has a claim at 1,200 ms; the Linux block has not been started; no repetition has a defect | `FAIL, INCOMPLETE`, 1 | The Linux block is started only on the owner's word: after that miss it is further measurement (record H). Until then no path is selected and nothing stands in for the platform that is absent |
| The same, and a diagnostic repetition stores an event twice | `CORRECTNESS_DEFECT`, 3, with the coverage `incomplete` and the budget axis `missed` printed | The defect decides the headline and the exit, and the miss and the absent platform stay on their axes. No path is selected: a platform is absent, and the data set is not eligible. The Linux block is started only on the owner's word (record H), and so is a data set that only fixes the defect (record F): a mandatory `valid` repetition missed |
| One mandatory C24 repetition on macOS, `valid`, has a claim at 1,200 ms, and the B24 set of five on macOS is `invalid` because a repetition does not show its backend; all else met and complete | `FAIL, INCOMPLETE`, 1 | The B24 set is run again only on the owner's word (record H). The old set and the miss are kept, and a replacement removes neither |
| One mandatory C24 repetition on macOS has a claim at 1,200 ms on a host that was not idle, so its set of five is `host_not_idle`; all else met and complete | `FAIL`, 1 | That set is run again only on the owner's word (record H): the miss it holds is a mandatory `host_not_idle` miss. The old set and the miss are kept |
| The B24 set of five on macOS is `invalid` because a repetition does not show its backend; no mandatory repetition that is `valid` or `host_not_idle` is over a limit; no repetition has a defect | `INCOMPLETE`, 2 | The set is run again as this section already allows, for a reason written before the run and with the old set kept; record H asks for nothing here, since no mandatory miss stands |
| The same, and a diagnostic repetition stores an event twice | `CORRECTNESS_DEFECT`, 3, with the coverage `incomplete` printed and the budget `not judged` for the `invalid` set | The defect decides the headline and the exit. No path is selected: the data set is not eligible. Neither "ask first" nor record H asks for anything here, since no mandatory miss stands; what this section already says of a defect and of an `invalid` set applies unchanged, and an `invalid` set holds no miss |

The names of the four axes and of their values, the headline words and the four exit numbers are
proposals of this revision; the schema that carries them is the performance engineer's.

**What a miss selects.** The owner ruled on this on 2026-11-21, and the ruling amends one sentence of the
direction of 2026-11-20, "If the revised budget also fails, the file fallback applies". In the ruling's
terms: every miss is kept and reported; a miss of the revised budget selects the file fallback for run
events only when it is attributable to events; a miss that occurs without events (claims alone, C0, a
burst alone) or on one platform alone selects no event path and goes back to the owner for a decision.
The rule below applies those words and adds no test of cause to them. It does not require that C0 of the
same block pass, it adds no cell that holds a burst without events, and it does not claim that a miss it
lets through was caused by events.

Whether a path may be selected at all is decided first, and the steps below are never read for a data set
that fails this. A data set is **eligible** only when all of these hold, taken over both required
platforms together:

- (a) Correctness is `ok` in every repetition, mandatory and diagnostic. None has a defect and none is
  `unknown`.
- (b) No required repetition stands as `invalid`, and none leaves its backend, its settings as read back,
  its timing check or its platform class not shown. A repetition that was `invalid` and has been replaced
  as "The verdict" allows is kept and listed and is not counted here; one that has not been replaced is.
- (c) Coverage is `complete`: every cell, repetition, platform, diagnostic, receipt and host record is
  present, and every repetition has its checkpoint coverage.

A data set that is not eligible selects **no path**: neither the fallback nor SQLite as the candidate. Its
headline and its exit are those of "The verdict" and this rule changes neither. Every miss in it stays a
failure, is reported on its own axis with every other axis, and is not used to choose. Nothing makes a
data set eligible by calling a defect incomplete, by leaving a miss out, or by dropping the repetition
that holds the defect: the defect is fixed in a new data set that names the old one, which is started
only on the owner's word when the data set also holds a mandatory miss ("The verdict"). A miss already
observed stays a failure whatever comes later. For an eligible data set, in this order:

1. The headline is `PASS`: SQLite is the candidate for events. The numbers and the results go to the
   owner, and nothing is accepted before the owner answers.
2. A miss in a population that holds no events selects no event path, and the whole result goes back to
   the owner. Such a population is every limit of C0, and in B24 a repetition in which every claim over a
   limit was due inside a burst's 10 s window and no event is over its age. A burst's 24 claims are
   committed one transaction each whether events exist or not, and no cell holds the burst without events,
   so a miss in the bursts alone is not told apart from a miss of claims alone.
3. A miss that only one platform shows selects no event path and goes back to the owner, even when its
   population holds events. A cell's miss is shown by both platforms only when each has a valid mandatory
   repetition of that cell that missed the same kind of limit in the same population. Two different
   kinds of miss in one cell, one on each platform, are each shown by one platform alone. The kinds are
   the five named below, and the match is made on the cell, the population and the kind together.
4. A miss on a host that was not idle is kept as a failure and selects no event path: it is not a valid
   timing sample ("The host"). It goes back to the owner with the host's record.
5. What is left is a valid miss in a population that holds events, which both platforms show, in an
   eligible data set that holds no miss of steps 2 to 4. The file fallback is selected, and the fallback as built is
   measured against this same budget (R63) before anything ships on it. That measurement is started only on
   the owner's word, and the selection starts none (below). No figure shows that it is faster.

Four things in those steps are not in the words of the owner's ruling. Three are this spec's reading where
the words stop: what "a burst alone" is in a cell whose bursts come with events; which kinds of limit and
population are told apart when "one platform alone" is judged by kind, as the owner answered (below); and
that one miss of steps 2 to 4 anywhere in the data set returns the whole choice to the owner. The fourth is
step 4, which reads nothing in the ruling: it comes from "The host" and is proposed with it. The second and
the third of the readings are proposals no longer: both were answered on 2026-11-21 under the owner's
delegation (record H of "Owner's answers"; below). The first reading and step 4 are still proposals. So is
the eligibility rule above the steps, which the method review asked for. The first and the third reading
send more to the owner and less to the fallback than the ruling's words alone. That is not claimed for the
second: the owner's words name no list of kinds.

*How "one platform alone" is judged: the owner's answer of 2026-11-21.* Two readings were put to the
owner. Under the cell reading, both platforms show a cell's miss when each has a valid mandatory
repetition of that cell that missed any limit. Under the limit reading, which the owner's record calls
the kind reading, they show it only when each missed the same kind of limit in the same population: the
regular claims, the claims of a checkpoint window, the claims of a burst window, an event's age, or a
refusal or something still queued. The limit reading sends more to the owner than the cell reading does.
The two differ in this case: on macOS one C24 repetition has a claim at 230 ms inside a checkpoint window
and nothing else over a limit; on Linux one C24 repetition has an event older than 5,000 ms and no claim
over a limit; everything else is met, both hosts are idle and the data set is eligible. The cell reading
would select the file fallback at step 5, from two different misses of which neither platform shows the
other's. The limit reading selects no path and returns the choice to the owner. The owner was shown both
readings with this case and answered "kind reading is fine" (record F of "Owner's answers"). Step 3
states that reading, and the cell reading is not adopted: in the case above no path is selected and the
result goes back to the owner. When the platforms share one kind of miss in a cell and one of them also
shows a kind that the other does not, that second miss is shown by one platform alone, and by the rule
for the whole data set (below) the whole choice goes back to the owner.

The answer chooses a reading and nothing more. It shows no cause: misses in one cell on two platforms,
of one kind or of two, do not show that events caused either, and the fallback still commits a row for
every batch and still checkpoints, so nothing shows that it would remove a miss. It accepts no number,
limit, method or result; the 230 ms and the 5,000 ms above are the values of the case and were not
measured. The list of five kinds was this spec's proposal when that answer was given: the record says that
the owner was shown the readings in plain words with the case, and does not say that the owner read the
list.

*The list of kinds and the whole data set: two answers of 2026-11-21, given under the owner's delegation*
(record H of "Owner's answers"). The five kinds are adopted, and they are operative. The match of step 3 is
made on three things together: the cell, the population and the kind. Whatever computes the selection reads
the kind of every miss and matches on it; a report that names the kinds beside a selection made without them
does not apply the reading. Every limit and every outcome under a kind stays in the evidence as it was
observed, and the report gives every population apart, so every kind is judged on its own figures.

A miss that step 2, 3 or 4 covers, anywhere in the data set, keeps the data set from selecting an event
path, and the whole result goes back to the owner. That is not a rule that every miss stops every path. An
eligible data set in which every miss is a valid miss in a population that holds events, and is shown by
both platforms in the same cell, population and kind, holds no miss of steps 2 to 4: it reaches step 5, and
the file fallback is selected. That holds for one such miss and for several, of one kind or of more than
one: each miss is matched on its own, and the data set is not held to one kind.

Three things are kept apart. Selecting a path names the event path that a data set points to, and nothing
more. It is not permission to measure: a data set that selects the fallback holds a mandatory miss, so the
measurement of the fallback as built is a further data set and is started only on the owner's word ("The
verdict"). And it does not qualify a path to ship: whichever path ships is itself measured as built, by the
same method and against the same budget, on macOS and on native Linux hardware before its PR merges, and a
path that misses goes back to the owner (R63).

The two answers accept no number, limit, eligibility rule, method or result, and they show no cause, as the
answer before them shows none. The owner confirms or changes the rest of this with the rest of this text. A
diagnostic selects nothing.

Cases that the rule must give, each with every other limit met and every other part complete:

| Case | Headline and exit | Path |
|---|---|---|
| macOS is complete and valid and one C24 repetition has a claim at 1,200 ms; Linux has not run | `FAIL, INCOMPLETE`, 1 | None: a platform is absent |
| C0 has a claim at 240 ms in a small checkpoint window, on both platforms | `FAIL`, 1 | None: no events; to the owner |
| In B24 one burst claim is at 201 ms in its window of 34 claims, on both platforms | `FAIL`, 1 | None: a burst alone; to the owner |
| C24 misses on macOS; Linux is complete and met everything | `FAIL`, 1 | None: one platform; to the owner |
| C24 misses the same kind of limit in valid repetitions on both platforms (on each, a claim over a limit inside a checkpoint window); C0 and the bursts met everything | `FAIL`, 1 | The file fallback |
| C24 misses on both platforms, and C0 misses on one | `FAIL`, 1 | None: to the owner. The C0 miss is a miss of step 2, and one platform alone shows it, so nothing is selected from the data set whatever both platforms show in C24 |
| Everything met; one repetition's host was not idle | `INCOMPLETE`, 2 | None |
| C24 misses on both platforms, in repetitions whose host was not idle | `FAIL`, 1 | None: to the owner, with the host's record |
| The size rule's calls complete at 250 s and 500 s and no timer call occurs; every limit met | `INCOMPLETE`, 2, with the reason `timer_moved_by_size_call` | None |
| The same, with one claim at 1,200 ms | `FAIL, INCOMPLETE`, 1 | None: the data set is not complete |
| A scheduled claim has a recorded refusal | `FAIL`, 1 | By steps 2 to 5 |
| A run is stopped at 512 MiB before its end record, and three claims have no record | `INCOMPLETE`, 2 | None |
| An event is stored twice | `CORRECTNESS_DEFECT`, 3 | None |
| A diagnostic repetition stores an event twice; C24 has a claim at 1,200 ms in valid repetitions on both platforms; all else met, complete and idle | `CORRECTNESS_DEFECT`, 3, with the budget axis `missed` printed | None: not eligible by (a). Both misses are kept and reported. The steps are not read, so step 5 selects nothing |
| A mandatory C0 repetition on macOS commits after a refusal; C24 misses in valid repetitions on both platforms | `CORRECTNESS_DEFECT`, 3, with `missed` printed | None: not eligible by (a); the misses are kept |
| One mandatory H32 repetition on Linux has no integrity result, so its correctness is `unknown`; C24 misses in valid repetitions on both platforms | `FAIL, INCOMPLETE`, 1 | None: not eligible by (a) and (c); the misses are kept |
| One mandatory B24 repetition on macOS does not show its backend and has a claim at 1,500 ms; it has not been replaced; C24 misses in valid repetitions on both platforms | `FAIL, INCOMPLETE`, 1; the 1,500 ms is listed as observed on invalid evidence | None: not eligible by (b) and (c); the C24 misses are kept |
| On macOS a C24 claim at 230 ms in a checkpoint window; on Linux a C24 event older than 5,000 ms; nothing else over a limit; eligible | `FAIL`, 1 | None: the platforms miss different kinds of limit, so each miss is shown by one platform alone; to the owner. That is the owner's answer of 2026-11-21 (the kind reading). The cell reading, which would select the file fallback here, is not adopted |
| On both platforms a C24 event is older than 5,000 ms in a valid repetition and no claim is over a limit; nothing else over a limit; eligible | `FAIL`, 1 | The file fallback: one kind (an event's age) in one population that holds events, in one cell, on both platforms, and no miss of steps 2 to 4. The measurement of the fallback as built still waits for the owner's word, and nothing ships on the selection |
| On both platforms a C24 claim at 230 ms in a checkpoint window and a C24 event older than 5,000 ms, each in a valid repetition; nothing else over a limit; eligible | `FAIL`, 1 | The file fallback: two kinds of miss, and each is shown by both platforms in the same cell, population and kind, so no miss of steps 2 to 4 stands. The match is made miss by miss and not once for the whole data set. The measurement of the fallback as built still waits for the owner's word, and nothing ships on the selection |
| On both platforms a C24 claim at 230 ms in a checkpoint window; on macOS also a C24 event older than 5,000 ms, and on Linux none; nothing else over a limit; eligible | `FAIL`, 1 | None: the event's age is shown by one platform alone (step 3), and one such miss anywhere returns the whole choice to the owner, although both platforms show the other kind |
| On macOS a C24 claim at 230 ms in a checkpoint window; on Linux an H32 claim at 230 ms in a checkpoint window; nothing else over a limit; eligible | `FAIL`, 1 | None: the kind is the same and the cells differ, so each miss is shown by one platform alone. The cell is part of the match; to the owner |
| C24 misses the same kind of limit in valid repetitions on both platforms; one more mandatory C24 repetition on macOS misses on a host that was not idle | `FAIL`, 1 | None: a miss of step 4 stands in the data set; to the owner, with the host's record |
| Every repetition of B24 ends `timer_moved_by_size_call`; every limit met | `INCOMPLETE`, 2 | None, and no new data set without the owner's direction or an approved revision of the method |
| A diagnostic of 10 events a second has an event older than 5,000 ms; the mandatory cells pass | `PASS`, 0, with the diagnostic disclosed | SQLite is the candidate |

**The host.** A timing sample is valid only on a host that was idle by a rule written before the run. That
is decision 22 of ADR 0013; this section follows it and does not amend it. The first run's report says
that its host was shared with other builds, and nothing here reads that run differently.

- Each repetition and each timing check runs while the harness holds the repository's host lease
  exclusively, taken in the ordinary way and waited for within its ordinary bound (ADR 0013 decision 21).
  The lease is cooperative, and holding it does not show that the host is idle, so the host is sampled too.
- Sampled every 15 s, from before the arrivals start to the end of the drain, as the gate samples: the
  one-minute load average, and the number of build processes (`cargo`, `rustc`, `rustdoc`,
  `clippy-driver`, `cargo-nextest`) that do not descend from the harness. Recorded as well, at the start
  and at the end: the power source, the thermal state where the platform reports one, and the device and
  the filesystem of the scratch directory.
- A repetition's host is **idle** when every sample is present, no sample shows a foreign build process,
  and no sample's load is above half the number of logical CPUs that the process may use. It is **not
  idle** when a sample breaks either limit, and it is treated as not idle when a sample is absent or the
  sampler stopped. Proposed and not measured: the 15 s, the half, and the list of process names. ADR 0013
  leaves the load threshold to the performance engineer, to be declared before the measurement; the half
  is that declaration as proposed, and the owner has not ruled on it.
- A repetition on a host that was not idle is kept whole, with its samples. If it met the budget it does
  not count toward a pass, and its cell is incomplete. If it missed, the miss stands: it is reported as a
  failure, it is never removed, averaged away or replaced by a later repetition, and it selects no event
  path ("What a miss selects"). The record gives the greatest load and the greatest count and names no
  cause: a miss beside a high load is not shown to be caused by the load.
- Idle, here, means idle by load and by build processes, and nothing more. The disk traffic of other
  processes (another program's syncs, an indexer, a backup) is not sampled, and a build shorter than the
  sample period can pass unseen, so a repetition recorded as `valid` on an idle host may still have
  shared the disk that its commits and checkpoints wait for. The record says "idle by load and build
  processes only" in those words. Whether each sample also records the write activity of the scratch
  device, without judging by it, is the performance engineer's to declare with the harness; this
  revision adds no sample and no limit for it.

**Diagnostics, which decide nothing.** Three repetitions of 660 s each, with the same checkpoint coverage,
reported against both budgets: C24 with a batch window of 10 ms and of 200 ms; C24 at 10 events a second
for each stream; C24 with `synchronous = FULL`; and C24 with one read held open from 295 s to 325 s. None
can rescue a mandatory miss or add one. A diagnostic that shows starvation, loss, corruption or growth
without bound goes to the lead and the owner even when the mandatory cells pass. A diagnostic without its
coverage is reported as incomplete coverage like any repetition. The cell at 10 events a second writes the
most and is the likeliest to have the size rule move its timer, and the mandatory cells C24, H32 and B24
are exposed to the same ("Checkpoints in every repetition"); whether any does is not known before it runs.

**The timing check.** Before each platform's block, a 10 s case with one claim a second holds the writer
for 1,500 ms at 1 s. The claim it delays must be reported over 1,000 ms and the counts must agree. A
platform whose check fails is not measured.

**Platforms and limits.** Two platforms are required. Each is named in the record with the evidence for
its class, and a platform whose class is not shown is unknown and leaves the result incomplete.

- *macOS hardware*: macOS running directly on Apple hardware and not in a virtual machine, with the
  scratch directory on the machine's internal storage and on a local filesystem. An external or a network
  volume does not qualify, and where the repository lies decides nothing about the scratch directory.
- *Native Linux hardware*: a Linux kernel running directly on a physical machine that is not the macOS
  machine, with no hypervisor between the kernel and the hardware. A guest of any virtual machine, a
  container on such a guest and a CI runner do not qualify, whatever they are called; a rented machine
  qualifies only when it is a whole physical machine. The scratch directory is on a block device attached
  to that machine and on a local filesystem: not a network, overlay, loop or memory filesystem. The
  harness runs on that kernel, outside any container that limits its CPUs, its memory or its I/O.
- *Evidence, recorded before a platform's first repetition*: the kernel or system build, what the system
  reports about virtualisation, the CPU model and the number of logical CPUs, the memory, any CPU or
  memory limit on the process, and the device, the filesystem and the mount options of the scratch
  directory. A name in a list of machines or of agents is not evidence.

These classes are this spec's reading of "macOS hardware" and "native Linux hardware" in R5, R63 and the
answer D6, and they are proposed with the method. No native Linux host has been named. A Linux container
on the macOS machine may be reported as a third result under its own name; it is a virtual machine on that
hardware and never stands for the Linux host. With no such host the result is incomplete, not passed, and
a miss that macOS has shown stays a failure meanwhile ("The verdict").

The build is the pinned toolchain with the library of ADR 0017 decision 19, in release mode, and its versions and digests are recorded. The entry experiment is the standalone harness and
names its backend `experiment`; R63's run of the path that ships names it `production` and refuses to start
on anything else. A repetition is stopped at 1,200 s with its setup, at 512 MiB resident or at 4 GiB of
scratch; it needs 10 GiB free at its start, and the disk floor of B11 holds throughout. The process runs
with a fresh home and configuration directory, an allow-listed environment, no project pointer and no
daemon, and it starts no agent CLI. Its results form a new data set and are never written into the first
run's files. The mandatory cells are about 6 h 25 min of traffic on each platform and the diagnostics about
2 h 45 min.

**How the proposal sits beside the other numbers of this spec.** Three numbers elsewhere in this spec do
not agree with the proposal as it stands. Each is answered here by saying what the two numbers measure and
what this spec proposes. None is answered by reading a measured result differently: the first run's FAIL
against 50 ms and 100 ms stands whatever the owner rules. The owner rules on all three with the numbers
(AP13), and until then nothing below is accepted.

- *Lane 1's 50 ms and the proposed 200 ms.* The 50 ms is the wait before a claim's transaction starts
  ("Writer lanes"). The first budget took the same number for the whole way from arrival to commit, and
  the first run missed that on macOS, where claims with no events took 10.9 ms at the 99th percentile. The
  proposed 200 ms also runs from arrival to commit: it holds the wait, the claim's transaction and its
  commit, and the first run saw a claim's own transaction take up to 499 ms. Proposed: the 50 ms stays in
  the lane table as the target for the wait alone and gates nothing; every run reports that wait apart,
  with how many claims exceeded it; and gate line 7 is read against the budget the owner accepts and
  against no other number. Not proposed: raising lane 1's number to 200 ms, which would loosen the target
  for the queue on account of the commit, a time the wait does not hold. If the owner accepts no revised
  budget, the first budget stays the one that gate line 7 is read against.
- *Lane 3's 1 s and the proposed 5,000 ms.* The 1 s is the deadline by which the writer orders an event batch
  against entity changes and maintenance (R26), and the first budget also made it a limit: no batch waits more
  than 1 s. The 5,000 ms is the age of one event from its arrival to its commit, and holds the batching, the
  wait and the transaction. It is there so that claims cannot pass by starving events, and its number is five
  times the first limit's, on a time that holds more. Proposed: the 1 s stays lane 3's deadline, unchanged in
  R26; the limit of the revised budget is the 5,000 ms; and every run reports how many events were older than
  1,000 ms. An event's age holds its batch's wait, so a count of zero shows that the first limit would have
  been met as well, a count above zero is the most that could have missed it, and the owner can set a
  stricter limit from the same data.
- *The experiment's 50 ms of batching and the shipped collector's 256 events or 250 ms.* The experiment
  batches all streams together over 50 ms, as the first run did, so that the two runs can be compared.
  The product's collector hands a run's batch over at 256 events or when its oldest event is 250 ms old
  ("Writer lanes"). The entry experiment is therefore no evidence about the collector as shipped. Its
  diagnostics at 10 ms and 200 ms bracket the experiment's own 50 ms and do not reach 250 ms. And at the
  rates of every cell, at most 10 events a second for a stream, a collector holds at most 3 events in
  250 ms, so the rule of 256 never acts and only the rule of age does. The evidence for the shipped
  collector is R63's measurement of the path that ships, with the collector's settings as shipped; at
  these rates it too shows the rule of age alone, and no latency figure in this spec touches the rule of
  256. Neither number of the collector changes on account of the experiment.

## Request keys

- A request frame may carry `key`: 16 to 128 characters of `A-Z a-z 0-9 _ -`. The CLI makes one per
  invocation and resends the request once with the same key when the connection drops after sending.
  `--request-key` and `ORCH_REQUEST_KEY` let a script supply its own. Keys apply to mutate and destructive
  operations whose change is a database transaction. A key on any other operation is ignored, and the CLI
  does not resend those: every read and control operation, and `db.backup`, `db.export` and `db.prune`, whose
  effect is a file or a bounded series of transactions.
- Scope: `(identity, project, run, epoch, key)`. The daemon takes run and epoch from the caller's lease, never
  from the request; they are empty and 0 for an owner session.
- The stored hash is SHA-256 over the operation name and its arguments in canonical JSON (sorted keys, no
  insignificant space).
- The row is written in the transaction of the change, with `expires_at` one hour later. A replay returns the
  stored result with `"replayed": true`. A result over 64 KiB is stored as `result_truncated = 1` with no body,
  and its replay returns `"replayed": true, "result": null, "result_truncated": true`.
- A replay never fails with `stale_revision`: the first outcome stands.
- `request_keys.expire` deletes expired rows every 5 minutes, at most 1,000 per transaction.
- `state = 'in_flight'` stays in the schema for 1.5; 1.1 writes only `done`.
- Never for credentials (R134): an operation that issues, rotates or enrols a credential takes no key, whatever
  its class. 1.1 has no such operation. A key sent with one is refused as `invalid_arguments` and is not resent.
  Authorization comes before the lookup: a principal that is refused gets no stored result.

## Protocol 3 and errors

`LOCAL_PROTOCOL` becomes 3 in the PR that first changes the wire (R50). Changes from 2:

| Change | Detail |
|---|---|
| Request frame | Optional `key`. |
| Terminal frame | Optional `replayed`, `result_truncated`. |
| Entities | Every entity object gains `revision`. |
| Changes | Optional `expected_revision` on every operation that changes one entity. |
| Lists | `limit`, `cursor`, `sort`, `with_count` in; `items`, `next_cursor`, `total` out. |
| Welcome | Optional `project` (the project id) and `generation`. The frozen fields, including `project_id {dev, ino}` of `<project>/.orchestry`, are unchanged. |
| New operations | `db.status`, `db.verify`, `db.query`, `metrics.get`, `attention.list`, `attention.get`, `policy.get` (read); `db.backup`, `db.export`, `attention.ack`, `policy.set` (mutate); `db.prune` (destructive). |
| Kept operations | All 111 stay. `state.get` returns the same fields as before, assembled from leases, retries and counters. |

| Code | Exit | When |
|---|---|---|
| `stale_revision` | 1 | `expected_revision` is not the stored revision. Execution `not_executed`. The message names the entity and both revisions. |
| `quota_exceeded` | 1 | The disk floor refuses new work (R85). Execution `not_executed`. Reserved for later storage limits. |
| `storage_unavailable` | 1 | The writer has failed (R128). Execution `not_executed`. The message names the SQLite result and says that a restart is needed. |
| `snapshot_expired` | 1 | A read was interrupted so that the write-ahead log could be shortened (R127). The read may be run again. |

A reused key with another body is `conflict`; an operation the matrix denies is `not_permitted`. Exits 1 to 7
keep their meanings: 1 general, 2 invalid arguments, 3 not initialized, 4 lock conflict, 5 adapter, 6
workspace, 7 no usable daemon. No exit code is added.

## Data directory

```
<data dir>/                      0700   one per daemon
  orch.lock                      0600   exclusive lock; never deleted; holds the holder's record
  orch.db  orch.db-wal  orch.db-shm   0600
  import-<random hex>.db         0600   only while an import from an export builds its database (R115); renamed to orch.db
  generation                     0600   the highest generation this directory has used; written and synced at every start
  blobs/                         0700
    sha256/ab/cd/<hash>          0600   ab and cd are the first four hex digits of the hash
    events/ab/<event file name>  0600   not in 1.1: the event fallback is not built (R63); 64 hex digits, ab the first two
    tmp/                         0700   file names generated by the daemon
  repos/                         0700   empty before 1.5
  spool/finish/<name>            0600   a run's finish until its transaction commits (R129); directories 0700
  spool/events-at-stop           0600   event numbers handed over and not committed when a stop ended (R139)
  spool/events-at-stop.tmp-<random hex>  0600   only while a version of that record is published (R139)
  restore-staging-<random hex>/  0700   only while a restore builds the state it will move in (R138)
  restore-in-progress            0600   only while a restore replaces the state (R138); nothing starts while it exists
  restore-in-progress.tmp-<random hex>  0600   only while a version of that marker is published (R138)
  backups/                       0700   one directory per backup, named by the daemon and created exclusively
    <backup>/                    0700
      manifest.json              0600   every file below, by name, size and SHA-256
      orch.db                    0600
      blobs/sha256/ab/cd/<hash>  0600
      blobs/events/ab/<event file name>   0600   not in 1.1: the event fallback is not built (R63)
      repos/                     0700   empty before 1.5
  run/                           0700
    orch.sock                    0600
    daemon.json                  0600
```

- The lock is taken before anything else is opened. The holder's record is JSON: kind (`daemon`, `import`,
  `restore`, `vacuum`), pid, start time, binary version and executable path. A refusal prints it (R30).
- Default location at creation: `<data home>/<name>/`, with `<name>` the project directory's base name in lower case,
  characters outside `a-z 0-9 . _ -` replaced by `-`, cut to 32 bytes, then `-` and the first 12 hex digits of
  the SHA-256 of the canonical project path. `orch init` and `orch import` record it in the project's record
  under `<data home>/projects/` (0600, one line; the file is named by a hash of the project id, R107), and from
  then on the project is found by its id, so moving the project directory does not lose its state (R34).
  `ORCH_DATA_DIR` names the directory outright.
- The socket follows 0012's rule: `run/orch.sock` when its absolute path is at most 100 bytes, otherwise the
  short path under `$TMPDIR`, recorded in `run/daemon.json`. `ORCH_SOCKET` still overrides.
- In the project directory 1.1 writes `.orch-project` (0644, at most 64 bytes; created exclusively and never
  through a link, R107) and, as 1.0 did,
  `.orchestry/workspaces/` and orch-peer's own files; `orch init` also writes the authored files
  `workspace-exclude` and `.gitignore` there, as 1.0 did. It writes no other path under `.orchestry/` (AP6).
  The directory `.orchestry/` itself must stay: its device and inode are the project identity of the frozen
  handshake. `orch init` creates it for a new project.
- A data directory that names another project id than `.orch-project` does is refused with both values
  printed. A project directory other than the recorded one is a move or a copy, handled as R34 says. The
  recorded project directory is a daemon-wide row (`daemon_config`) that `orch init` and `orch import` write
  from the directory they ran for, so an export never carries one (R105).

## Filesystem names

Which values may select a file (R101 to R108, R117, R118). The security review's finding S2: the first
event-file layout, `blobs/events/ab/<run id>`, resolved outside the data directory for the run id
`../../../../outside-canary`. The wire type `Id` bounds only the length of an id and the domain keeps ids as
plain strings (verified at `fb6a1e1`: `crates/orch-proto/src/bounded.rs`, `crates/orch-domain/src/run.rs`), so
no layout may rest on what an id contains.

**Rule.** A value that reaches the daemon from outside is data, and is never a path component. "From outside"
means a request, a 1.0 source file, an export file, a backup's manifest, and any row of a database that an
import or a restore brought in. Below the data directory, in a backup and in the data home's `projects/`,
every path component has one of four forms, or is a shard directory named by the first two hex digits of
the hash below it:

| Form | Where | What it is |
|---|---|---|
| Fixed | The layout in "Data directory", a backup's included | The literal names there. |
| Content hash | `blobs/sha256/ab/cd/<hash>` | 64 lower-case hex digits: the SHA-256 of the file's bytes. A hash column is parsed to this form before every use; another value is a typed error and no file is touched. |
| Derived hash | `blobs/events/ab/<event file name>` (not in 1.1: the event fallback is not built, R63), `spool/finish/<finish file name>` (R129), `<data home>/projects/<record name>` | 64 lower-case hex digits of a SHA-256 the daemon computes over ids, as below. |
| Generated | `blobs/tmp/`, `backups/<UTC time>-g<generation>/`, `backups/pre-restore-<UTC time>/`, `backups/pre-migration-v<from>-<UTC time>/` (R124; the version is the integer of `schema_migrations`), `import-<random hex>.db` | Random hex digits made by the daemon, or a time and a generation it formats itself. A backup directory is created exclusively: a name that exists is an error and is never reused. |

- **Event file name.** The SHA-256, in lower-case hex, of these bytes in order: the ASCII text
  `orch-event-file-v1`, one zero byte, the length of the project id in bytes as eight bytes big-endian, the
  project id, the length of the run id as eight bytes big-endian, the run id. The lengths make the pair
  unambiguous whatever bytes an id holds. 1.1 creates no event file, because the event fallback is not
  built (R63). The name stays defined and its vectors below keep their values, and they are due only with
  the fallback, as every other check of an event file is.
- **Finish file name** (R129). The same, with the text `orch-finish-file-v1` in place of the first: for
  project `prj_test1` and run `run_test1`,
  `7a402d9d06dbc9a9c1e288e8ee7d2636bdab972350a11cb513de22ca059a8ae6`. The other label keeps it from ever
  being an event file's name. The file stands directly in `spool/finish/`, with no shard directory. It is
  the one name that 1.1 builds from a run id, so in 1.1 the hostile run ids are proved on it: the
  finish-file rows of the vectors below, as a name and through the operations that write the file, recover
  it and remove it (R129).
- **Project record name.** The SHA-256, in lower-case hex, of the project id's bytes.
- An export is one file; inside it, blob bytes are rows keyed by their hash, and nothing is a path (R108).
- A name found in a hashed directory that has not its form (a stray file) is reported by `orch db verify`;
  the sweep and the pruning never open or delete it.
- The original name of an attachment, a template's name, a context key, a run's recorded workspace path and
  the like are columns. They are shown escaped and never opened.
- Two things are not values from outside in this sense. A path the owner gives on a command line or in the
  environment (`ORCH_DATA_DIR`, `--from`, `--to`, a backup to restore) is the owner's choice of a root and is
  used as given. And the default name of a data directory ("Data directory") is made from the project
  directory's base name by replacing every character outside `a-z 0-9 . _ -` and adding a hash, which makes
  it one name by construction.

**Access** (R102). One module reaches every file below the data directory (the lock, `generation`, `run/`,
the blobs, the finish spool, the event files, the backups), below the data home's `projects/`, in a backup
being restored and in an import source, and writes `.orch-project` and the authored files of the project
directory.

- It opens its root once. The data directory is checked as R29 says; the root of a source or of a backup
  must be a directory and not a link. From there each directory and each file is opened by one name relative
  to its parent's descriptor, refusing a link. A path string of more than one component is never built below
  a root, and the module itself refuses a name that is empty, `.`, `..` or holds a separator or a NUL,
  whatever its caller passed.
- A new file is created exclusively, 0600 (`.orch-project` 0644); a new directory 0700. A delete names its
  parent's descriptor and one name. A rename names the old parent's descriptor and one name and the new
  parent's descriptor and one name; it replaces whatever is at the new name and never follows it, so a link
  planted there is replaced, not written through.
- An entry is judged by `fstat` on the descriptor just opened, never by an earlier look at its name: it must
  be a regular file, or a directory where the layout has one. A file that could be a FIFO is opened without
  blocking and then judged.
- More than one hard link is refused where it can do harm: on every file of an import source and of a backup
  being restored, which are trees from elsewhere, and on a file opened to be appended to or changed in place
  (an event file, the lock, `generation`), where a second link would let the write land in another file. A
  read below the data directory does not look at the link count: the bytes are checked against their hash
  (R58, and each event batch against its row), and a snapshot of the data directory made with hard links
  must not stop the daemon. A backup copies bytes and never makes a hard link.
- Two exceptions. SQLite opens `orch.db` and its two companion files itself, by their fixed names, and makes
  and removes the companions as it runs, and likewise the scratch database of an import from an export
  (R115), by its generated name; the socket is bound by its path under 0012's rules. What protects
  these is the 0700 directory with its owner check (R29) and the exclusive lock. Before SQLite opens the
  database the daemon also refuses an `orch.db` that is a link or not a regular file, as defence in depth
  and not as a guarantee.
- Not covered: a hard link to a directory. An unprivileged user cannot make one on the filesystems 1.1
  supports.

**The project directory** (R106, AP14). One id still names a directory:
`<project>/.orchestry/workspaces/<task id>`, 1.0's layout, which AP6 keeps so that existing workspaces and
what agent CLIs remember about them stay valid. It is the only place where the daemon's storage or engine
uses an id as a path component; orch-peer's own files keep their 1.0 rules, which this goal does not change
(B15). The id must be one name of `A-Z a-z 0-9 . _ -` other than `.` and `..` (1.0's check, which refuses and
never rewrites), an imported task id has already passed R104, and the directory is reached without following
a link at `workspaces` or at the entry. A stored workspace path is never used. The branch of a local run
(`runs.branch`) is merged or deleted only when it is the name the daemon itself would give that task's run.

**Workspace ownership** (R117, R118). Those checks show that an id is one name. They do not show that a
directory is one task's. The re-review of S2 measured it (finding S2-R1): `tsk_Case` and `tsk_case` both
pass, are two rows with two leases, and on a filesystem that folds case are one directory, so the second task
read the first one's files and its cancellation would have removed them (F14). Two rules close it, and neither
changes an id.

- **Fold key.** The id with `A` to `Z` read as `a` to `z`. It is compared and nothing else: never stored in
  place of an id, never shown as one, never a path component.
- **One task for each fold key** (R117). A project holds no two task ids with one fold key, on every platform
  and whatever its filesystem tells apart: a unique index in the schema, the same comparison in the file
  reference. The daemon makes every new task id itself and draws again when one is refused. So one row, one
  lease and one name stand for one directory and one branch wherever case is all the filesystem folds, and a
  database, a backup or an export that one supported platform accepts is accepted on every other.
- **Before publication** (R117). An import from a 1.0 directory, an import from an export and a restore
  compare the task ids they bring with one another and with the names that the project's `workspaces/`
  lists, read for the names alone. Two ids of one fold key are refused (`id_alias`). A listed name with a
  task's fold key belongs to that task only when it is the id byte for byte, and only when the source can
  have made it: a 1.0 directory holds its own workspaces beside its tasks, and a backup is taken, for now,
  to be the state of this project, so for those the exact name is the task's workspace, as AP6 keeps it. An export carries no
  workspace, so a directory already at one of its tasks' names is not that task's. Any other match is
  refused (`workspace_alias`), with the directory untouched and unentered. Whether a backup may be taken
  as this project's own state is open: it is the fifth point of AP14, the owner's choice between two
  options, and the clause for a backup stands only until the owner has ruled. An exact name, a device
  and an inode show which directory a name reaches. They do not show who wrote a backup, and neither
  does the directory the owner chose or a manifest whose hashes agree. The rule about processes is a
  separate one and is settled in the text: a backup's process records cause a refusal and a report, and
  never a signal (R100). A listed name that matches no
  task is reported and left; no rule here adopts it or removes it.
- **At every use** (R118). The listing decides, not the name: the daemon opens `workspaces/<task id>` and
  then requires the listing to hold exactly that name for the directory it opened. A filesystem that folds
  a name keeps the spelling it was created under, so a directory reached through another spelling is seen
  as another name's and is neither reused nor removed. A new workspace is an exclusive create, which fails
  when anything answers to the name. A branch is acted on only under the exact name in the repository's own
  listing of refs. This is the rule that holds on a filesystem whose folding the fold key does not know
  (one that drops a trailing dot, or does not keep case): there a step fails with `workspace`, and no
  directory is shared or deleted.
- **Across platforms.** The fold-key rule does not ask what the destination folds, so nothing changes when a
  project, an export or a backup moves between a filesystem that folds case and one that does not. Its
  cost is stated in AP14: a 1.0 project kept on a filesystem that tells case apart, holding two task ids
  that differ only in case, is refused by the import until the owner removes one of the two tasks with the
  1.0 binary; nothing rewrites either id.

**Vectors.** The project id is `prj_test1` unless a row says otherwise. A row that gives a path must produce
exactly that path, below `blobs/events/` for an event file, below the data directory for a finish file and
below the data home for a record; every other row must end as it says, with nothing created, changed or
removed outside the root. 1.1 creates no event file, so the six rows that give an event file's path and the
two that plant a link at an event file are due only with the event fallback, which 1.1 does not build (R63).
They keep their values. In 1.1 the same run ids, and `..`, are proved on the finish file, the one file that
1.1 names from a run id: the seven finish-file rows. Their names come from the label `orch-finish-file-v1`
and have no shard directory, so no value of an event-file row is a value of theirs. Each is tested as a
name, and through the operations that write the file, recover it at a start and remove it, with a canary
outside the root unchanged (R129).

| Input | Expected |
|---|---|
| Run id `run_test1` | `e9/e995144062a58ba0a060bba97a77a3dfe8562cafe55abdcf6aea6a80c0a1f5b2` |
| Run id `../../../../outside-canary` | `14/145203cc529e79216d2d1848e916dad67b2972dcecabd60172da207064e6a4f0` |
| Run id `/outside-canary` | `6e/6e13989f0d6780be172792bc54b6c9513304f9e53be8777383474adbf37b9ba2` |
| Run id `a`, one zero byte, `b` | `44/44a3e7f93016ec1f693dbfe7b41c0c2bcf4e7065afd41be1acbebe6f30cb1fd4` |
| Run id `a/b` | `71/71aa219cde03650bb527933f5f618fb4035c29e142949542a822485ceb1119e6` |
| Project id `prj_test`, run id `1run_test1` (the first row's two ids joined and split at another point; the lengths make the hash differ) | `06/06eeaf803b9df10bdf787a8b27ae39242dd2ea1d492a2e107fdd872008bae0b4` |
| Finish file, run id `run_test1` | `spool/finish/7a402d9d06dbc9a9c1e288e8ee7d2636bdab972350a11cb513de22ca059a8ae6` |
| Finish file, run id `../../../../outside-canary` | `spool/finish/300053d414157031bb39555fcccdfb92cee8e3e09aa295ef82c003a48deeb8c8` |
| Finish file, run id `/outside-canary` | `spool/finish/318a5d16cb6133a09a5a8af597ae8e142b783e201bd72ddb11bf64a609416f61` |
| Finish file, run id `a`, one zero byte, `b` | `spool/finish/f816d7b547b5d3226225a4e844e8f0ea3f145136efbd160981562b8b8fcc9f5e` |
| Finish file, run id `a/b` | `spool/finish/026c08d56cbb044bf5df5b48f6431c20dacc214fda32ebbb2af85ab402ddee41` |
| Finish file, run id `..` | `spool/finish/93143077dd25bea2a3b43f38cff5c6edfc691d35366fe3e9bb1c1af31eff0c2a` |
| Finish file, project id `prj_test`, run id `1run_test1` (the first finish-file row's two ids joined and split at another point; the lengths make the hash differ) | `spool/finish/a14c156547860418c7880716bab69d8a08dfb1b23a9fa7cea2714e430fb32262` |
| Project record for `prj_test1` | `projects/9fa0423cd44dfbc56bc5b71dea05c66fb0f1f2f0d884eed107e15cfdbce760c7` |
| A blob hash column holding `../x`, 63 hex digits, or upper-case hex | Refused; no file touched |
| A symbolic link at `blobs/events/zz` or at an event file's name, on append, read and prune | Refused; the link's target unchanged |
| A hard link to an outside file at an event file's name, on append | Refused before any byte is appended |
| A hard link to an outside file at a blob's name, on read | The read fails unless the bytes hash to the name; nothing is written |
| A hard-linked file in an import source or in a backup being restored | Refused before any byte is read |
| A name passed to the module that is empty, `.`, `..`, `a/b`, or holds a NUL | Refused by the module; no call reaches the filesystem |
| A link planted at a blob's final name before the rename from `blobs/tmp/` | The link is replaced by the blob; its target is unchanged |
| A file `blobs/sha256/ab/cd/notes.txt` | Reported by `orch db verify`; never opened or deleted by the sweep |
| A FIFO or a socket at a blob's name | Refused, and the caller does not block |
| A second exclusive create of an existing name | Refused; the first file unchanged |
| A backup manifest naming `../outside-canary`, an absolute path or a name with a NUL | Backup refused before any change (R108) |
| A source document whose id is `../../../../outside-canary`, or a task `..` | Import stops and names the file (R104) |
| A 1.0 index entry or attachment name `../../outside-canary` | Looked up among the listed names, found absent, reported; never opened (R103) |
| `.orch-project` holding `../../x`, two lines, 65 bytes, or being a symbolic link | Exit 2, naming the file (R107) |
| `orch init` or `orch import` where `.orch-project`, `.orchestry/.gitignore` or `workspace-exclude` is a link to an outside file | The command stops; the link and its target are unchanged (R102, R107) |
| A project record that is a link, holds a relative path or two lines, or names a directory open to others | Exit 2, naming the record (R107) |
| A run whose `branch` is `main`, or `orchestry/<another task's id>/x` | Not merged and not deleted (R106) |
| An export holding a task id `..`, `a/b`, or one with a NUL | Import refused before the row is stored (R108) |
| Task ids `tsk_Case` and `tsk_case` in one project: the second created through the API; both in a 1.0 source, in an export, in a backup | The create is refused by the storage and the daemon draws another id; the import and the restore are refused before publication, `id_alias`, naming both. Neither id is changed (R117) |
| `tsk_case` and `tsk_cas3` in one project; `tsk_Case` in `prj_test1` and `tsk_case` in another project | Accepted |
| A 1.0 source with the task `tsk_case` and a directory `workspaces/tsk_Case` holding a canary | Import refused, `workspace_alias`, naming the id and the entry; the directory not opened, the canary unchanged (R117) |
| A 1.0 source with the task `tsk_case` and `workspaces/tsk_case`; with `workspaces/tsk_gone` and no such task | The first stays that task's workspace; the second is reported, left in place and never adopted or removed (R117) |
| An export with the task `tsk_case`, for a project directory that holds `workspaces/tsk_case`, or `workspaces/tsk_Case` | Refused before publication, `workspace_alias`; the directory unchanged (R117) |
| A backup with the task `tsk_case`, restored where `workspaces/tsk_Case` exists; a backup whose database lacks the index of R117 | Refused before any change (R117) |
| Preparing `tsk_case` where `workspaces/tsk_Case` holds a canary, on a filesystem that folds case | Fails with `workspace`; nothing reused, created or removed; the canary unchanged (R118) |
| The same on a filesystem that tells the two apart | `workspaces/tsk_case` is made by an exclusive create; `workspaces/tsk_Case` and its canary are unchanged (R118) |
| Cancelling `tsk_case`, failing it for good, and its cleanup, in the folding case | Each `workspace.remove` ends having removed nothing; the canary unchanged (R118) |
| A name that the listing returns in another spelling than the one just created | The daemon removes the empty directory it made and fails with `workspace` (R118) |
| A branch `orchestry/tsk_Case/x` that no run recorded, loose or packed, and a run of `tsk_case` whose branch is `orchestry/tsk_case/x` | Not merged and not deleted, and no branch created beside it; its commit unchanged (R118) |

## Authorization matrix

One function in the dispatcher (R52). In 1.1 every real session is the local owner (AP3); worker and task
principals exist for tests and for the port's caller checks. The security review of this spec confirms the
task column before the dispatcher PR.

| Operations | Owner | Worker | Task |
|---|---|---|---|
| Reads of the caller's project in the 1.0 catalogue; `subscribe`, `unsubscribe`, `request.cancel` | allow | deny | allow |
| `task.create`, `message.send`, `message.broadcast`, `message.mark_read`, `context.set` | allow | deny | allow; the row is stamped with the task's run |
| Every other mutate operation of the 1.0 catalogue | allow | deny | deny |
| Destructive operations | allow, when the session mode allows them | deny | deny |
| `dispatch.*`, `daemon.stop`, `transfer.*` | allow | deny | deny |
| `db.*`, `metrics.get`, `attention.*`, `policy.*`, any configuration write | allow | deny | deny |
| Port calls bound to a lease the caller holds, with its current epoch (append events, renew, release, finish) | allow | allow | deny |
| Anything for a project that is not the caller's | deny | deny | deny |
| Anything bound to a lease with an older epoch | deny | deny | deny |

Session modes (`read_only`, `allow_destructive`) and the MCP allowlist of 1.0 still narrow the owner column.
The new operations are in no MCP allowlist.

## Import rules

- `orch import [--from <project directory or export file>] [--dry-run] [--resume] [--assume-stopped <run id>]... [--json]`. It runs with both
  daemons stopped, under the data directory's lock and the 1.0 owner lock (R31, R68). It is offline: it does
  not use the socket (AP7). The check that comes before every write (R112) holds the 1.0 owner lock only: a
  data directory that does not exist yet is created and locked at the first write, and one left by a stopped
  import is locked before the check.
- Bounds (R66, R109 to R112): "Import bounds" has every limit, inside a document as well as on its bytes,
  and what each refusal leaves behind. In short: a YAML document at most 1 MiB and a JSON document at most
  16 MiB, each measured for depth, nodes and expansion before anything is built from it; an event line at
  most 64 KiB; an attachment at most 64 MiB; at most 1,000 rows per transaction; and memory that does not
  grow with the project. The import checks the whole source before it writes anything, so a file over a
  bound is reported by its path relative to the project and nothing is created. Nothing is cut short
  silently: an event line that cannot be stored is a recorded gap.
- Source access (R103): the importer opens the source `.orchestry/` once, without following a link, and lists
  the directories of the table below from that descriptor, one level each and two for `attachments/`. It
  opens an entry only by the name the listing gave, as "Filesystem names" says for every file. It never opens
  a path made from a value inside a document: an index entry, a task's attachment name and the like are
  looked up among the listed names, byte for byte, and one that is not there is a dangling reference. A
  listed entry must be a regular file with one hard link where the table expects a file, and a directory
  where it expects a directory; a symbolic link, a FIFO, a socket, a device or a file with more than one
  hard link there stops the import before it is read. A directory the table does not name is reported as
  one entry and not entered. `workspaces/` is listed one level for its names alone (R117): no entry of it
  is opened, entered or judged, its entries count toward the listing's bound, and what an entry is, is
  decided when a task uses it (R118).
- Ids (R104): an entity's id is the stem of its file. The document's own id or key must equal it, and the stem may
  hold only `A-Z a-z 0-9 . _ -`; otherwise the import stops and names the file. Ids are kept as they are
  (R69), as data: no file below the data directory is named by one ("Filesystem names").
- Task ids together, and the workspaces that exist (R117): two task ids of the source that differ only in
  the case of ASCII letters are refused as `id_alias`, and a name in `workspaces/` that folds onto a task's
  id without being it as `workspace_alias`, at the check and so before anything is written (R112). Neither
  id is changed. A workspace under a task's exact name stays that task's (AP6); one that no task names is
  listed by the dry run and left in place. The check keeps one fold key for each task, which the listing's
  bound limits.
- Nothing outside the database (R105): the import writes rows, blobs and `.orch-project` (and the empty
  directory `.orchestry/` when an export is imported for a directory that has none). It writes no lease
  and no intent, signals no process, looks one up only to ask whether its pid or its process group exists (R119), and removes no workspace. A recorded pid, a run's
  `workspace_path`, a task's stored workspace and a project directory path are stored as text (the pid and
  the workspace path in `runs.local`) or left out. Nothing reads them in order to act: only a lease leads to
  a signal, and only a task id to a workspace.
- Order: `position` follows each `_index.json`. An entity on disk that its index does not list is appended in
  id order and reported; an index entry with no file is reported and skipped. The index gives membership and
  order and nothing else (R122): an entity's content is its own file's, which 1.0 writes first (F16), and an
  entity whose index entry differs is listed as `index_differs`. An index that is missing, is not an array
  or does not parse lists nothing, so its directory is imported in id order and listed as `index_unreadable`.
  An `_index.json` is judged by the first line of this table that applies to it, so that the outcome never
  depends on the order of an array's entries:

  | # | Found | Class | The import |
  |---|---|---|---|
  | 1 | The listing of its directory has no `_index.json` | `index_unreadable` | Goes on in id order |
  | 2 | It is listed and is not a regular file with one hard link (R103) | The file-kind stop | Stops before the file is read |
  | 3 | An input or output error while it is opened or read. An interrupted read is tried again first and is not an error | A file the import cannot read | Stops |
  | 4 | More bytes than the bound ("Import bounds") | `bytes` | Stops |
  | 5 | The one pass over its bytes meets a depth or a node budget before it meets anything that is not JSON | `depth`, `nodes` | Stops |
  | 6 | It was read in full within its bounds and is not JSON | `index_unreadable` | Goes on in id order |
  | 7 | It is JSON and not an array: an object, null, a scalar | `index_unreadable` | Goes on in id order |
  | 8 | An array with an entry, anywhere in it, that is not an object or has no string identity | `index_entry` | Stops, naming the index. Drafted from the default of T21-X1 |
  | 9 | An array whose every entry has a string identity, with one identity more than once | `index_duplicate` | Stops, naming the index and the identity. Drafted from the default of T21-X2 |
  | 10 | Any other array, the empty one included | A usable index | Goes on: order from the index, and `index_differs`, an entity it does not list and an entry with no file as above |

  Lines 2 to 5 are terminal wherever they are met and are never turned into `index_unreadable`: a file the
  import could not read or measure is not a file it may decide to ignore. At the check each of them ends
  as a refusal does, exit 2 with nothing created; after the first write, as R112 says for a stop, exit 1.
  Lines 8 and 9 are decided on the whole array (R122). An entry without a string identity anywhere makes it
  line 8, whatever else the array holds, which is also what 1.0 does with it: 1.0 rebuilds such an index
  and so drops its duplicates (F19). Only an array whose every entry has a string identity can be line 9.
  Line 9 is the import's line and not 1.0's. 1.0 uses such an array as it stands only where every entry is
  a complete entity of the directory's type, and rebuilds it where one is not, as it rebuilds
  `[{"id":"b"},{"id":"b"}]` (F19). The import decodes no entry, so it cannot tell the two apart: it stops
  for both, and its refusal states both cases and says of neither that it is this index's.
  Identities are compared byte for byte, so two that differ only in case are two identities here (R117
  judges the task ids of the files, not the entries of an index), and the comparison is of identities and
  not of files: an identity that stands twice and has no file still stops. The identity is `id`, and `key`
  under `context/`. Nothing else of an entry is judged: an entry with a string identity and otherwise wrong
  or missing fields gives order and is listed as `index_differs`, although 1.0 would rebuild that index.
  Decoding entries as their entity type to copy 1.0 exactly would make the index's content an input again,
  and 1.0's own index reader writes to the directory it reads (F19), so the import runs neither.
- Runs: `run_no` and `epoch` count from 1 per task in start order; `task_epochs.next_epoch` is one more than
  the largest. `attempt_no` is 1.0's attempt number where it is unique within the task; a run that would
  repeat one is stored uncounted with the 1.0 number in `local`, and the dry run counts such runs. The rendered prompt becomes a blob.
  A task's recorded branch is stored as the `branch` of that task's last run; it is text until R106 has
  checked it. Events get `seq` from their line number; a line that does
  not parse becomes a gap. Model and adapter version are empty. Every event is imported; the pruning job
  applies the retention afterwards.
- `state.json`: the retry queue becomes retry rows and totals become counters. Running entries are first
  checked for work that may still be going on (R119, F15, F17). A local entry whose pid names a process
  that exists or a process group that still has a member, and every remote entry whatever the peer registry
  says, refuse the import
  and are named: importing them would send their tasks out a second time while the first run still works.
  The group is asked about because 1.0 makes each agent the leader of its own group and a 1.0 stop can
  leave a child of the agent running after the leader has gone. The check asks only whether the process
  and the group exist. It delivers no signal, and the import has no option that stops a
  process: a 1.0 entry holds no start time, so the pid may be another program's. The owner ends such work
  with the 1.0 daemon (let the run finish or cancel its task, then stop the daemon), or states with
  `--assume-stopped <run id>` that a named run has ended; where that run's process or group still exists
  the import shows it and asks again. Every entry that passes, claimed entries included,
  is closed as 1.0's startup recovery closes one whose process is gone, in the database only (R105, AP14).
  A closed remote run is not attached again, and a late outcome for it changes nothing (R120).
- Tasks that are `done` get `landing_state = 'skipped'`; 1.0's recorded proof goes to `legacy_proof`.
- `config.yml`: R70. If the unknown keys exceed 64 KiB the import stops and names them by size. What the
  file holds decides the rest, and the six cases are kept apart because 1.0's effective reader ran the
  project on its defaults alone in four of them and on the mapping's keys over its defaults in a fifth,
  and refused the sixth (F18):

  | `config.yml` | 1.0 ran the project on | The import |
  |---|---|---|
  | Missing: the listing of the source has no such entry | Its defaults | Imported as the empty mapping. No problem is listed. R109 does not apply, since there is no stream to count |
  | A stream of no document: zero bytes, spaces and newlines only, comments only | Its defaults | Refused under `documents`, limit 1, observed 0, saying that 1.0 read the file as its defaults. Drafted from the default of T21-C1 |
  | One document that is null: `null`, `~` | Its defaults | Stops as a document that does not fit its type, saying the same. Drafted from the default of T21-C2 |
  | One document that is a scalar or a sequence: `42`, `[1]`, `"x"` | Its defaults | Stops as a document that does not fit its type, saying the same. The stop was this spec's before 2026-11-22; what the refusal says is new |
  | One document that is a mapping, `{}` included | The mapping's keys over its defaults | R70: each key the mapping sets. A key it does not set: T21-O1, below |
  | Two documents or more | Nothing: 1.0 refused the file | Refused under `documents`, limit 1, observed 2 |

  The file kind (R103), the byte bound, an input or output error, and a file that is not UTF-8 or does not
  scan as YAML come before the count of documents: such a file stops for that reason whatever it holds, and
  it is never read as a missing or an empty one. A missing file is one the listing does not hold; a file
  that is listed and cannot be opened is an input or output error. The count of documents comes from the
  one pass of R109, on the bytes that pass read; no second parse decides it. Whether that pass counts a
  bare `---` as one document, which 1.0 reads as null (F18), is unverified; vector C-V6 pins it, and under
  the two defaults the file stops either way. An entity file is not `config.yml`: a task, an agent, a goal
  or a team whose stream holds no document is refused under `documents` and 1.0 could not read it either
  (F18), so nothing is said about 1.0 there.

  A key that the mapping does not set (T21-O1; proposed, not answered). O7's answer makes
  `scheduling.max_concurrent_agents` the optional `run_cap`. Neither that answer nor R70 as it stood when
  the owner gave it says what an absent key gives, and no other answer in the record does. Two readings:

  | Source `config.yml` | 1.0 ran at most | A, proposed: the value 1.0 ran with is carried | B: only the keys the file sets are imported |
  |---|---|---|---|
  | Missing | 6 local runs at a time | `run_cap` 6 | No `run_cap` |
  | `{}` | 6 | `run_cap` 6 | No `run_cap` |
  | A mapping without the key, such as `project: {name: p}` | 6 | `run_cap` 6 | No `run_cap` |
  | A mapping with `scheduling: {max_concurrent_agents: 2}` | 2 | `run_cap` 2 | `run_cap` 2 |
  | The file as 1.0's `orch init` wrote it, which sets the key to 6 | 6 | `run_cap` 6 | `run_cap` 6 |

  This text is drafted from A: the import carries what `read_merged` gave 1.0, which is the mapping over
  1.0's defaults, and the dry run says for `run_cap` that the 6 is 1.0's default and not the file's. That
  line is a note and no problem: it is no class of "Import problem classes", it stops nothing, and the dry
  run still lists no problem for the file (R65; vectors C-V1, C-V7 and C-V14). Why A
  is proposed: under B a project that 1.0 held to six local runs is imported with no cap of its own, in an
  import that is one way, and that is found out only when more agents start at once; a cap that is kept is
  seen in the dry run and lifted with one setting. It is the reasoning of B14, which keeps an `auto`
  project running as it ran. What A costs: the importer holds a copy of 1.0's defaults that must never
  follow a later change of 1.1's, and an imported project cannot tell a value its owner chose from one
  that 1.0 supplied. B is R70 read to the letter, needs no such copy, and leaves `run_cap` as optional as
  its name says. Where 1.0's default and 1.1's are the same value the two readings do not differ in effect;
  `run_cap` is the one setting this spec names where they differ, and no other setting was traced for it.
  "No `run_cap`" is read here as no cap of the project's own on its runs at one time: this spec calls the
  setting optional and says nothing more about leaving it unset, so that reading is unverified. A file that
  1.0 wrote sets every key (F18), so the two readings differ only for a missing file and for one a person
  cut down. The owner ratifies A or chooses B; until then no caller is built on either (plan task T21).
- Text over 64 KiB in a description, message or context value becomes a blob (R97) and is listed by the dry
  run.
- A finished import writes `.orch-project` and records its time and source in `daemon_config`. A stop between
  the two is repaired by running the command again (R125). orch never deletes a 1.0 file. `orch db status`
  shows that the 1.0 files are still present and how large they are (O3).
- After the import (R121): the import keeps the size and hash of `state.json` and of each `_index.json`. A
  1.0 daemon that runs on the project later rewrites them, and the next 1.1 start then refuses and names the
  files, because tasks may have run under both. `--accept-legacy-change` is the owner's word that the 1.1
  database stands; it records the files as they are and starts with dispatch off. Nothing under
  `.orchestry/` is written for this check.
- Import problem classes (R65, R123). "Stops" means the check stops and nothing is written (R112):

  | Class | The import |
  |---|---|
  | A document it cannot read: it does not parse, or does not fit its type | Stops, naming the file |
  | An input or output error while a source file is read, after an interrupted read has been tried again | Stops, naming the file. Never read as a missing file or as `index_unreadable` |
  | A `config.yml` whose stream holds no document (the budget row below), or whose one document is not a mapping (the first row) (R70) | Stops, naming the file and saying that 1.0 read it as its defaults (F18). Drafted from the defaults of T21-C1 and T21-C2; for a scalar or a sequence the stop is not new |
  | No `config.yml` (R70) | Goes on as the empty mapping; no problem is listed |
  | A budget of "Import bounds" refused, except an event line | Stops, naming the file, the rule and the limit |
  | An event line over its bound or that does not parse | Goes on; a recorded gap |
  | A link, a special file or a file with more than one hard link where a file is expected (R103) | Stops |
  | An id that is not its file's stem or is outside the alphabet (R104) | Stops |
  | `id_alias`, `workspace_alias` (R117) | Stops |
  | `in_flight_live`, `in_flight_remote` (R119) | Stops, unless each run is named with `--assume-stopped` |
  | Unknown configuration keys over 64 KiB; a template over 64 KiB | Stops |
  | `index_differs`, `index_unreadable`; an entity its index does not list; an index entry with no file (R122). `index_unreadable` is an index that is missing, is not an array, or was read in full within its bounds and is not JSON | Goes on, as "Order" above says; listed |
  | `index_entry`: an index array with an entry that is not an object or has no string identity (R122) | Stops, naming the index and saying that 1.0 rebuilt such an index from the entity files (F19). Drafted from the default of T21-X1 |
  | `index_duplicate`: an index array whose every entry has a string identity, with one identity more than once (R122) | Stops, naming the index and the identity and saying that 1.0 used such an index as it stood only if every entry was a complete entity of the directory's type, and otherwise rebuilt it from the entity files (F19). The refusal does not say which of the two 1.0 did with this index, and the stop is the same in both. No occurrence is chosen. Drafted from the default of T21-X2; the name is a proposal |
  | A dangling reference; an unknown key; a workspace that no task names; a path with no rule | Goes on; listed |
  | Text over 64 KiB in a field | Goes on; stored as a blob and listed |

  1.1 has no option that skips a file or sets one aside (AP17). A file that stops the import is the owner's
  to repair, after taking an untouched copy of `.orchestry/` (R136). For the rows added on 2026-11-22 the
  refusal names the repair. A `config.yml` with no document, or a null one: write `{}` into it, which is
  what 1.0 took it for. `index_duplicate`: remove the extra entry to keep the index's order, or remove the
  index to accept id order. `index_entry`: remove the index; the import then goes on in id order, as it
  does for an index that does not parse.
- Every 1.0 path has a rule (R99):

  | Path under `.orchestry/` | At import | After the import |
  |---|---|---|
  | `tasks/`, `agents/`, `goals/`, `teams/`, `messages/`, `context/`, each with its `_index.json` where it has one | Rows | Unused |
  | `runs/run_*.json`, `runs/run_*.jsonl` | Runs, results, events and prompt blobs | Unused |
  | `state.json` | Retry rows and counters; in-flight entries closed | Unused |
  | `config.yml` | Settings, and `legacy.config` for unknown keys (R70) | Unused |
  | `attachments/<task>/` | Blobs and `task_attachments` rows; the directory is matched to a task and each file to the task's list by listed name, and the original file name is a column | Unused |
  | `templates/*.md` | `templates` rows named by the file stem, byte for byte; a file over 64 KiB stops the import by name | Unused. As in 1.0 the engine does not read them (F11); `orch init` stores the `default` row where 1.0 wrote the file |
  | `workspace-exclude`, `.gitignore` | Not imported | Used in place: authored files of the project directory, which `orch init` still writes |
  | `workspaces/` | Not imported; listed one level for its names alone, which R117 compares with the task ids; no entry opened or entered | Used in place (AP6), found by task id as R106 says, and only once R118 has shown the directory to be that task's |
  | `remotes.yml`, `remotes/`, `peer-id` | Not imported | orch-peer's own files, used in place |
  | `orchestry.lock`, `.daemon-owner.lock`, `daemon.json`, the socket | Not imported | The 1.0 daemon's; 1.1 holds the owner lock without writing it (R31) |
  | `logs/` | Not imported | Unused; 1.0 creates it and nothing was found that writes it (F11) |
  | Anything else | Not imported; named by the dry run and by the import's report | Unused |

- An export file (R80) is a SQLite database of the same schema version holding one project's rows from the
  project tables, without `leases`, `intents` and `request_keys`, plus a table of blob bytes in chunks and a
  manifest with a hash of every table. The file named by `--from` is opened without following a link and must
  be a regular file. It is read as a hostile file: "Import bounds" has its format, how it is opened, what is
  checked before a row is copied, and that the import publishes a database only when all of it has passed
  (R113 to R115). Its rows are data as a 1.0 source's are (R105, R108): a blob is stored under the hash of
  its bytes, the project id passes R107's check, a task id passes R104's, and a run it holds unfinished has
  no lease in the new directory and is ended at the first start (R41). Its task ids pass R117 together, and
  it brings no workspace: a directory that already stands in the project's `workspaces/` under one of its
  tasks' names, in any spelling, refuses the import before the database is published and is left as it is.

## Import bounds

What its input can make `orch import` do (R109 to R116). The security review's finding S1: the first bounds
limited a file's bytes and nothing inside it. A document of 40,001 bytes can nest 20,000 deep and one of 412
bytes can stand for 100,000,000 scalars, both far inside a byte bound; and an export file is an arbitrary
SQLite database, whose own manifest of hashes shows that it agrees with itself and not that it can be trusted.

**Rule.** A source is measured before anything is built from it. Every limit is checked on the bytes or on
the parser's events, by code whose memory and stack do not depend on what the input says about itself, and
only an input inside every limit reaches a deserialiser, a row or a blob. No limit is a clock: a deadline
cannot stop work it does not control, which is why 1.0 removed its own (F12). A source is the owner's choice
of a root, but its content is not the owner's word: a 1.0 directory arrives with a checkout and an export
file from another machine.

### A 1.0 directory

| Source | Bytes, at most | Inside it, at most |
|---|---|---|
| A YAML document: `tasks/`, `agents/`, `goals/` and `teams/` `*.yml`, `config.yml` | 1,048,576 | Exactly one document in the stream; depth 64; 64 aliases; 200,000 nodes and 4,194,304 bytes as expanded; no alias inside its own anchor; no explicit tag |
| A JSON document: `runs/run_*.json`, `messages/*.json`, `context/*.json`, `state.json`, each `_index.json` | 16,777,216 | Depth 64; 1,000,000 nodes |
| One line of `runs/run_*.jsonl`; the file itself has no bound and is read as a stream | 65,536, the newline not counted | Depth 64; its bytes already bound its nodes |
| An attachment | 67,108,864 | Not parsed |
| A template | 65,536 | Not parsed |
| The listing of the source (R103) | — | 1,000,000 entries in all |

How each is counted, so that two implementations agree at the limit:

- **Bytes.** What the importer reads, not what a directory entry or `fstat` reports: a reader stops at the
  bound and one byte more, and that byte is the refusal.
- **Depth.** The sequences and mappings (for JSON, arrays and objects) open at one time. A document that is
  one scalar has depth 0. For YAML it is the depth of the document as expanded: at an alias, the depth at
  that place plus the height of what its anchor expands to, so a chain of shallow anchors that expands deep
  is refused.
- **Nodes.** A scalar is 1; a collection is 1 and its children; a mapping key is a node; an alias is as many
  nodes as its anchor expands to (1.0's rule, F12). In JSON every value is a node and a member's name is not.
- **Expanded bytes.** The bytes of every scalar, keys included; an alias counts what its anchor expands to.
- **Aliases.** Every alias in the stream, whether it stands as a value or under a merge key (`<<`). The node
  and byte budgets count each of them at its full expansion, so the two uses draw on one budget (R110).
- **Tag.** A node with an explicit tag of any kind is refused: a custom one (`!x`), a standard one (`!!str`,
  `!!binary`), the non-specific `!`, on a scalar, a collection or the document's root.
- Every count is added with a check for overflow and tested at each addition; a count that would wrap is over
  budget. The pass keeps one frame for each open collection and one count for each anchor, so its memory is
  bounded by the document's bytes and never by what the document expands to.

Compatibility with 1.0 (R110, AP15). A document inside the budgets is read as 1.0 read it: aliases
expanded, merge keys resolved. The YAML budgets are 1.0's own reader's where it has one:

| Budget | Limit | 1.0's reader (F12) | A document 1.0 read that the import refuses |
|---|---|---|---|
| Bytes | 1,048,576 | The same | None |
| Documents in the stream | Exactly 1 | At most 1: a stream of no document is read as null (F18) | A `config.yml` whose stream holds no document (zero bytes, spaces and newlines only, comments only), which 1.0 read as its defaults. No entity document: 1.0 cannot read a task, an agent, a goal or a team from a stream of none |
| Aliases | 64 | The same | None |
| Nodes as expanded | 200,000 | The same | None |
| An alias inside its own anchor | Refused | The same | None |
| Depth as expanded | 64 | 128, by its parser library and not by a rule of its own | One nested 65 to 128 deep. A task document that 1.0 writes nests three deep |
| Bytes as expanded | 4,194,304 | No rule; about 65 MiB could be built from one file | One whose aliases repeat its scalars past 4 MiB. Without an alias a document expands to no more than its own bytes |
| Explicit tag | Refused | Ignored by typed fields | One that a person tagged by hand; the tag never had an effect |

Nothing 1.0 writes holds an alias, a merge key or a tag, so only a document edited by hand can meet the
last three rows; its refusal names the rule and says that 1.0 read the document. The row of the document
count is the fourth place where the import is stricter, and until 2026-11-22 this table said "The same"
and "None" there, which was wrong for `config.yml` (F18). 1.0's `orch init` writes a mapping, so only a
`config.yml` that a person or another tool emptied meets it; its refusal names `documents`, the limit 1 and
the 0 it found, and says that 1.0 read the file as its defaults. That the import refuses such a file, and
does not read it as the empty mapping, is drafted from the unanswered default of T21-C1. A `config.yml`
whose one document is not a mapping is inside every budget and is no row of this table: it stops on its
type, with the same statement ("Import rules"). 1.0 has no limit on a JSON
document; the JSON limits are new. An index of one million entries is about the largest that 16 MiB admits.

Order and outcome (R112):

1. **Check.** The import first does everything its dry run does: it lists the source (R103), reads every
   document within its byte bound, checks the budgets on the bytes it holds, and only then deserialises those
   same bytes. It writes nothing meanwhile; the data directory is not created.
2. **Load.** Only a source with no refusal is loaded. Each file is read and checked again in the same way
   before its rows are written, so a file that changed after the check cannot bring in unchecked bytes.

| Found | When | Outcome |
|---|---|---|
| A document, an attachment or a template over a budget; a listing over the entry limit | The check | Refused: exit 2. Nothing is created or changed below the data home or in the project directory; on a `--resume`, nothing beyond what the stopped import left |
| Two task ids of one fold key; a name in `workspaces/` that R117 does not let a task own | The check | Refused, as the row above, under `id_alias` or `workspace_alias` |
| An event line over its bound, nested too deep or not parsing | Either step | Not a refusal. The line is not stored, lines like it that follow are one gap with it (`run_event_gaps`), and the dry run and the report count them for each run (R111) |
| A document over a budget that passed the check (the file changed) | The load | A stop: exit 1. The transaction in progress is rolled back and nothing of the document is stored. The import is unfinished: the daemon refuses to start on it and `--resume` continues (R67) |
| Free space under the floor, an input or output error, a stopped process | The load | The same stop |

A document that is inside every budget and still does not parse or fit its type is a file the importer
cannot read (R65); this section does not change how that is handled. The `--json` report lists each refusal
as `path` (escaped, relative to the project), `rule`, `limit` and `observed`, the first 1,000 of them and a
count of the rest. `rule` is one of `bytes`, `documents`, `depth`, `aliases`, `alias_cycle`,
`expanded_nodes`, `expanded_bytes`, `tag`, `nodes`, `entries`, `id_alias`, `workspace_alias`, `index_entry`,
`index_duplicate`, or one of the export rules below. `observed` is the value at which the pass stopped,
which is the least the document
holds, since the pass does not go on to measure a document it has refused. For `id_alias` and
`workspace_alias` (R117), `path` is the second task's file, or the entry of `workspaces/`, `observed` is the
other id, and `limit` is empty; for an export, `path` is the task's id. For `index_entry` and
`index_duplicate` (R122), `path` is the `_index.json`, `limit` is empty, and `observed` is empty for
`index_entry` and the repeated identity, escaped, for `index_duplicate`: the first one that repeats, in
the array's order. A refusal that says what 1.0 did with the file says it in the report, as R110 requires
of each place where a budget is stricter than 1.0's reader. For `index_duplicate` that statement is the
two cases of R122, and it names neither as this index's.

### An export file

The format, which `orch db export` writes and `docs/storage.md` records (R80):

- One SQLite file of 4,096-byte pages in the rollback-journal format, with no companion file. Its header
  holds the application id 1330791256 (the bytes `ORCX`) and the user version 1, the version of this format.
- Tables and nothing else: the project tables of the product's schema version with their own definitions,
  without `leases`, `intents` and `request_keys` and without any daemon-wide table; `export_manifest`; and
  `export_blob_chunks`. The only indexes are the automatic ones of the tables' keys.
- `export_manifest` holds the format version, the schema version, the project id, the row count and a
  SHA-256 of the rows of each table, and the number of blobs and their total bytes, and no other row. It
  finds damage and truncation. It does not make a file trustworthy, and no check below is left out because
  the manifest agrees.
- `export_blob_chunks` holds every blob's bytes as rows of (`sha256`, `chunk_no`, `bytes`), written in
  ascending order of hash and then of chunk number. A blob's chunks are numbered from 0 without a gap; every
  chunk but its last is exactly 1,048,576 bytes; the last is 1 to 1,048,576 bytes, or 0 bytes as the only
  chunk of an empty blob. Blob bytes are therefore read in bounded pieces and never as one value.

How the importer reads one (R113, R114), in this order:

1. **Header.** It opens the file as R102 says and reads the first 100 bytes itself: the 16-byte SQLite magic
   string, page size 4,096 (offset 16), file-format versions 1 and 1 (offsets 18 and 19; 2 is the
   write-ahead-log format, which an immutable connection would read without its log, F13), user version 1
   (offset 60), application id 1330791256 (offset 68), and a file size that is a whole number of pages.
   Anything else is refused as `export_header` before SQLite sees the file.
2. **Connection.** A connection of its own, opened read-only with `immutable=1`, so that SQLite reads this
   one file, takes no lock and looks at no journal or log beside it. It is never attached to the connection
   that writes the destination. Set through the library before any statement runs: defensive mode on; the
   schema untrusted (`SQLITE_DBCONFIG_TRUSTED_SCHEMA` off); triggers and views off; the limits of the table
   below; the authorizer and the progress handler of step 3. The first statements are then the pragmas
   `hard_heap_limit`, `mmap_size = 0`, `cell_size_check = ON` and a page cache of 8 MiB, none of which reads
   the file's content. Extension loading is never turned on, and the library's `load_extension` feature
   stays off (ADR 0017 decision 19).
3. **Authorizer and progress handler.** The authorizer denies everything but: the four pragmas of step 2
   and `quick_check`; `SELECT`; a read of the columns `type`, `name`, `tbl_name` and `sql` of
   `sqlite_schema`; and a read of the named columns of the format's tables. So no function runs, nothing is
   attached, written or defined, and no other pragma runs. The progress handler is called every 1,000 steps
   of the virtual machine and inside `quick_check`; it stops a statement when the import is cancelled, and a
   table scan when it has used more than 200 steps for each row the manifest states for that table, and
   10,000 more.
4. **Integrity.** `PRAGMA quick_check(1)` is the first statement that reads the file's content. Any answer
   but `ok` is `export_integrity`. It is what makes the scans below finite: it finds a page that is
   referenced twice, which a scan does not (F13).
5. **Schema.** `SELECT type, name, tbl_name, sql FROM sqlite_schema` must return, row for row, the set this
   binary holds for the format at its own schema version; one row more, fewer or different is
   `export_schema`. The file's schema is compared and never adopted: the destination is created by the
   binary's own migrations.
6. **Manifest, rows, blobs.** One statement for each table, `SELECT <the named columns> FROM <table> NOT
   INDEXED`, a plain scan with no join, sort, function or index; no statement is built from text the file
   holds. Rows go to the destination in transactions of at most 1,000 rows or 8 MiB, by inserts with bound
   values, so the destination's own types, checks, keys and foreign keys judge them. Before that the
   importer refuses: a value of a project table over 65,536 bytes (R97 keeps larger text in the blob store,
   so the product's own export holds none); text that is not UTF-8; a `project_id` other than the
   manifest's; an id that fails R104 or R107 (R108); a task id with the fold key of one already read
   (`id_alias`, R117); a table with a row more or fewer than the manifest
   states, where the scan stops at the first row too many; and a chunk outside the chunk rules. A blob's
   chunks are written to `blobs/tmp/` and hashed as they arrive; the file gets its name only when the bytes
   hash to the stated name and their number is the size its `blobs` row states. Before publication every
   `blobs` row has its file and every blob the file carried has its row.

| Run-time limit (`sqlite3_limit`) | Value | SQLite's default |
|---|---|---|
| `LENGTH`: a string, a blob or a row | 2,097,152 | 1,000,000,000 |
| `SQL_LENGTH` | 100,000 | 1,000,000,000 |
| `COLUMN` | 100 | 2,000 |
| `EXPR_DEPTH` | 20 | 1,000 |
| `PARSER_DEPTH` | 100 | 2,500 |
| `COMPOUND_SELECT` | 3 | 500 |
| `VDBE_OP` | 25,000 | 250,000,000 |
| `FUNCTION_ARG` | 8 | 127 |
| `ATTACHED` | 0 | 10 |
| `LIKE_PATTERN_LENGTH` | 50 | 50,000 |
| `VARIABLE_NUMBER` | 10 | 999 |
| `TRIGGER_DEPTH` | 10 | 1,000 |
| `SCHEMA`: objects in the schema | 128 | 10,000,000 |
| `WORKER_THREADS` | 0 | 0 |
| Hard heap limit, for the import's process | 268,435,456 | none |

The values are the "high-security" column of SQLite's guidance for untrusted input, except `LENGTH` (a
1 MiB chunk and its row must fit), `EXPR_DEPTH` and `SCHEMA` (room for the product's own tables and checks),
and `WORKER_THREADS`, which that list does not name.
They bind the product's own export too: R80's test imports one under them, and a limit is raised only by
editing this table in the same PR. The heap limit is for the whole process, which is why it is set in the
offline import and nowhere in the daemon.

Publication (R115). An import from an export is all or nothing, and has no resume:

1. Steps 1 to 5 above and the manifest run before the data directory is created or locked. A file refused
   there leaves nothing anywhere.
2. The importer then takes the data directory (R30, R68; it must hold no `orch.db`), removes a scratch
   database that a killed attempt left, and builds the database as `import-<random hex>.db` there, a
   generated name in the sense of "Filesystem names", which SQLite opens itself like `orch.db`.
3. When every table, count, hash and reference has passed, and no name in the project's `workspaces/` has
   the fold key of a task the export brings (`workspace_alias`, R117), it records the import and the
   export's SHA-256 in the database, closes it so that it is one file, renames it to `orch.db` and syncs
   the directory. Only then does it write the project's record and `.orch-project`.
4. A refusal (exit 2, an export rule) or a stop (exit 1) before the rename leaves no database, no record and
   no pointer. The scratch file is removed. A blob already written is an unreferenced file named by the hash
   of its bytes: it selects nothing, a later import of the same bytes reuses it, and the sweep removes it
   (R60). A stop after the rename leaves a complete database without its record or pointer; running the
   same import again finds it by the export's hash, writes the two and changes nothing else.

The export rules of the report: `export_header`, `export_integrity`, `export_schema`, `export_manifest`
(a count, a hash or a version that differs), `export_row`, `export_blob`, and `export_limit` (a run-time
limit, the heap limit or a step budget stopped a statement). `id_alias` and `workspace_alias` are refusals
of an export as they are of a 1.0 directory. A dry run of an export does steps 1 to 6 without a
destination, so it reports everything but what only the destination's keys and foreign keys find; it makes
R117's two comparisons itself, since it has no index to make the first.

What these rules do not do: they do not make SQLite's own reading of a hostile file a thing this project
has proved. The settings above are the ones SQLite documents for a file from elsewhere, and the library's
C code is trusted to keep to them.

### Vectors

Each row must end as it says. A refused row also leaves the source byte-identical and the destination as
R112 or R115 says, within R116's time and memory. The corpora are made by the tests: documents from a seed
(R90), export files by altering the bytes of an export the product wrote; none comes from a real project.

| Input | Expected |
|---|---|
| The review's document: 20,000 `[`, one `0`, 20,000 `]` (40,001 bytes), as a task, as `config.yml`, as a run and as `state.json` | Refused, `depth`; no more than 65 collections are opened |
| The review's alias document (412 bytes): `a0: &a0 [x]`, then `a1` to `a8`, each a sequence of ten aliases of the one before | Refused, `expanded_nodes`, at the fiftieth alias |
| 1.0's document of 84,661 bytes and 60 aliases that expands to 2^30 nodes | Refused, `expanded_nodes` |
| `a0: &a0 [x]`, then `a1` to `a62`, each one sequence around an alias of the one before (62 aliases; depth 2 as written, 64 as expanded) | Accepted |
| The same with `a63` | Refused, `depth` |
| One anchored mapping of 4,000 entries merged by `<<` into 64 mappings | Refused, `expanded_nodes`: a merge key draws on the same budget |
| `defaults: &d {a: 1, b: 2}`, `one: {<<: *d, b: 3}`, `two: {<<: *d}` | Accepted; `one` is `a: 1, b: 3` and `two` is `a: 1, b: 2`, as 1.0 read them |
| A YAML document of 1,048,576 bytes; of one byte more | Accepted; refused, `bytes` |
| 64 aliases of one short scalar; 65 | Accepted; refused, `aliases` |
| A document that expands to 200,000 nodes; to 200,001 | Accepted; refused, `expanded_nodes` |
| A document that expands to 4,194,304 bytes; to one more | Accepted; refused, `expanded_bytes` |
| `a: &a [*a]` | Refused, `alias_cycle` |
| `id: !custom abc`; `id: !!str abc`; `data: !!binary YWJj`; `id: ! abc`; a tag on a sequence; a tag on the root | Each refused, `tag`, with the note that 1.0 read it |
| Two documents in one file | Refused, `documents` |
| A document of anchors that double at each of 70 levels (2^70 nodes as expanded) | Refused, `expanded_nodes`, at the level where the count first passes the budget, long before a count could wrap |
| An `_index.json` that is one array of 999,999 strings (1,000,000 nodes); of one string more | Accepted; refused, `nodes` |
| A JSON document nested 64 deep; 65 deep | Accepted; refused, `depth` |
| A JSON document of 16,777,216 bytes; of one byte more | Accepted; refused, `bytes` |
| An event line of 65,536 bytes; of one byte more | Stored; a gap |
| One event line of 256 MiB with no newline | One gap; at most 65,537 bytes of it held at a time |
| Three lines in a row that are over-long, not UTF-8 and not JSON, between two events | Two events and one gap |
| A listing of one entry more than the limit | Refused, `entries`; no document read |
| 2,000 refused documents in one source | Exit 2; 1,000 listed and a count of 1,000 more |
| A task file replaced by the depth document between the check and the load | Exit 1; nothing of that task stored; `--resume` completes once the file is put back |
| 4,096 random bytes; a copy of a product `orch.db`; an export with file-format bytes 2 and 2; with pages of 512 bytes; with user version 2 | Each refused, `export_header`; SQLite never opens the file |
| A valid export with files `-journal`, `-wal` and `-shm` planted beside it | Imported exactly as without them; the three files unread, and no file created beside the export |
| An export in which one interior page names the same child twice | Refused, `export_integrity` |
| An export with one table, index, view, trigger or virtual table more; with a generated column; with a changed `CHECK`; with a table missing; with `leases`, `intents` or `request_keys`; with `identity_tokens` | Each refused, `export_schema` |
| An export with 10,000 tables more | Refused, `export_schema` or `export_limit`; resident memory within R116 |
| An export whose trigger or view would write an outside canary or call `load_extension` | Refused, `export_schema`; the canary unchanged |
| An export with a stored value of 3 MiB | Refused, `export_limit` |
| An export with a text value of 65,537 bytes in a project table; with text that is not UTF-8; with a row of another project | Each refused, `export_row` |
| An export with one row more, or one row fewer, than its manifest states | Refused, `export_manifest`; at most one row past the stated count is read |
| A chunk of 1,048,577 bytes; a short chunk that is not the last; chunk numbers 0, 2; hashes out of order; bytes that do not hash to the name; a `sha256` of `../x` | Each refused, `export_blob`; no file gets a name it did not earn |
| A blob that no `blobs` row names; a `blobs` row with no chunks | Refused, `export_blob` |
| A row that breaks a key or a foreign key of the destination, as the last row of the file | Refused, `export_row`; no `orch.db`, no record, no pointer |
| The import killed at each batch boundary and just before the rename | No `orch.db`; the next import of the same file succeeds |
| The import killed between the rename and the pointer | The same import run again writes the record and the pointer and changes nothing else |
| The product's own export of the generated project, with blobs of 0, 1,048,576 and 1,048,577 bytes | Accepted under every limit; every row and hash reproduced (G4) |

### Vectors for `config.yml` and for an index

The cases of "Import rules" for `config.yml` and for an `_index.json`, each with the test that proves it. The
tests are in `crates/orch-cli/tests/import.rs` (`import::`) unless a name says otherwise, and plan task T21
creates them: until then the names are forward references. Every input is synthetic and made by the test.
Every row leaves the source byte-identical (R64), the index included, and the dry run agrees with the
import's check (R112). A row that stops or is refused ends at the check, exit 2, with nothing created below
the data home. "As `{}`" means that every imported setting, `legacy.config`, `run_cap` and `preapproval` equal
those of C-V7, and that the dry run lists no problem for the file; the note of T21-O1 on a value that is
1.0's default is not a problem. `parse`, `type` and `io` are the bounded
reader's own diagnostics for a document that does not scan, that does not fit its type, and that met an input
or output error. They are not report rules of this spec: how the report words a file the import cannot read
is R65's, and this revision does not change it. The column "Rests on" names the unanswered point that a row
is drafted from ("Open questions"); a row with none holds under every answer.

| # | `config.yml` | Expected | Rests on | Test |
|---|---|---|---|---|
| C-V1 | No file | As `{}` | | `a_missing_config_imports_as_the_empty_mapping` |
| C-V2 | Zero bytes | Refused, `documents`, limit 1, observed 0; says that 1.0 read the file as its defaults | T21-C1 | `a_config_with_no_document_is_refused_and_says_1_0_read_it` |
| C-V3 | `" \n  \n"` | As C-V2 | T21-C1 | The test of C-V2 |
| C-V4 | `"# synthetic config\n"` | As C-V2 | T21-C1 | The test of C-V2 |
| C-V5 | Whitespace with a tab at the start of a line, which 1.0's scanner rejects as well (the sample of issue 146's evidence) | Stops, `parse`; says nothing about 1.0. A file that does not scan is never counted as a stream of no document | | `a_config_that_does_not_scan_stops_before_its_documents_are_counted` |
| C-V6 | `"null\n"`; `"~\n"`; `"---\n"` | Each stops and says that 1.0 read the file as its defaults. `"null\n"` and `"~\n"` stop as `type`. `"---\n"` stops as `type` if the pass counts it as one document and as `documents` if it counts none; which it does is unverified, and the test records it | T21-C2 | `a_null_config_stops_and_says_1_0_read_it` |
| C-V7 | `"{}\n"` | The baseline: imported, and no problem listed for the file | T21-O1, for what the baseline holds | `an_empty_mapping_config_is_the_baseline` |
| C-V8 | `"scheduling: {max_concurrent_agents: 2}\n"` | `run_cap` 2; every other value as C-V7 | | `a_partial_config_imports_its_keys_over_the_baseline` |
| C-V9 | `"{}\n---\n{}\n"` | Refused, `documents`, limit 1, observed 2; says nothing about 1.0, which refused it too | | `a_config_of_two_documents_is_refused` |
| C-V10 | 1,048,577 bytes of comment | Refused, `bytes`: the byte bound comes before the count of documents | | `a_config_over_the_byte_bound_is_refused_for_its_bytes` |
| C-V11 | A zero-byte task, agent, goal and team file | Each refused, `documents`, observed 0; says nothing about 1.0, whose reader refuses each (the test runs that reader on a disposable copy) | | `an_entity_file_with_no_document_is_refused_as_1_0_refuses_it` |
| C-V12 | `"42\n"`; `"[1]\n"`; `"\"x\"\n"` | Each stops, `type`, naming the escaped relative path and saying that 1.0 read the file as its defaults | | `a_config_that_is_not_a_mapping_stops_and_says_1_0_read_it` |
| C-V13 | The refusals of C-V2 to C-V4 and of C-V11 | The text of each holds `expected one document, found 0` | | `source_budget::a_stream_of_no_document_is_refused` |
| C-V14 | No file; `"{}\n"`; `"project: {name: p}\n"` | `run_cap` is 6 in each, and the dry run says that the 6 is 1.0's default, as a note and with no problem listed for the file | T21-O1 (proposal A) | `a_key_the_config_does_not_set_takes_the_value_1_0_ran_with` |

For an index: a directory with the entity files `a`, `b` and `c`. The same rules hold with the identity
`key` under `context/`, and the tests run both.

| # | `_index.json` | Expected | Rests on | Test |
|---|---|---|---|---|
| X-V1 | Missing | `a, b, c`; `index_unreadable`; goes on | | `an_unreadable_index_is_rebuilt_in_id_order` |
| X-V2 | `[` | As X-V1 | | The test of X-V1 |
| X-V3 | `{}`; `null` | As X-V1 | | The test of X-V1 |
| X-V4 | `[0]`; `[{}]`; `[{"id":1}]` | Each stops, `index_entry`, naming the index and saying that 1.0 rebuilt such an index | T21-X1 | `an_index_entry_without_a_string_identity_stops` |
| X-V5 | Two inputs. `[{"id":"b"},{"id":"b"}]`; and an array that holds the whole entity `b` twice, each entry as 1.0 writes one | Each stops, `index_duplicate`; names the index and `b`, each escaped; says that 1.0 used such an index as it stood only if every entry was a complete entity of the directory's type and otherwise rebuilt it; says the same for both inputs, and of neither which of the two 1.0 did; chooses no occurrence. Beside the import, the test runs 1.0's reader of the directory's type on a disposable copy of each input, in each of the five directories that have an index, and shows that it rebuilt the first and used the second (F19) | T21-X2 | `a_repeated_index_identity_stops_and_names_it` |
| X-V6 | `[{"id":"b"},{"id":"b"},0]`; `[0,{"id":"b"},{"id":"b"}]` | Both stop as `index_entry` and neither as `index_duplicate`: the class does not depend on the order of the entries | T21-X1 | `a_malformed_index_entry_decides_the_class_wherever_it_stands` |
| X-V7 | `[{"id":"z"},{"id":"z"}]`, and no file `z` | Stops as the first input of X-V5 does, with the same statement, naming `z` | T21-X2 | `a_repeated_index_identity_with_no_file_still_stops` |
| X-V8 | `[{"id":"A"},{"id":"a"}]` | Not a duplicate. `A` is listed as an entry with no file; `a` is placed first; `b, c` are appended and listed as not in the index | | `index_identities_that_differ_in_case_are_two` |
| X-V9 | `[]` | A usable index that lists nothing: `a, b, c`, each listed as not in its index; not `index_unreadable` | | `an_empty_index_array_is_usable` |
| X-V10 | `[{"id":"b"},{"id":"zzz"}]` | `b`, then `a, c`; `zzz` listed as an entry with no file | | `an_index_entry_with_no_file_is_listed_and_skipped` |
| X-V11 | `[{"id":"../x"}]` | `../x` listed, escaped, as an entry with no file; no path is made from it (R103) | | `import_hostile::an_index_identity_is_never_made_into_a_path` |
| X-V12 | 16,777,217 bytes; nested 65 deep; 1,000,001 nodes | Stops under `bytes`, `depth` and `nodes`; never `index_unreadable` | | `an_index_over_a_budget_stops_and_is_never_unreadable` |
| X-V13 | A permission error injected in the middle of the read; one interrupted read injected | The first stops, `io`, and is never `index_unreadable`. The second is tried again and gives the result of an undisturbed read | | `an_index_read_error_stops_and_an_interrupted_read_is_retried`, a unit test of the `import` module of `orch-infra`, where a reader can be injected |
| X-V14 | A symbolic link; a FIFO; a file with two hard links | Each stops by R103 before it is read | | `import_hostile::an_index_that_is_not_a_regular_file_stops_before_it_is_read` |

What an explicit answer of the owner would change in these rows, each through a PR that also changes the
rule it rests on:

- T21-C1 answered A: C-V2 to C-V4 become "As `{}`", and C-V13 keeps C-V11 alone.
- T21-C2 answered A: C-V6 becomes "As `{}`" for a document the pass counts as one null.
- T21-X1 answered A: X-V4 and X-V6 become "As X-V1".
- T21-X2 answered B: X-V5 and X-V7 become "As X-V1". This is not recommended.
- T21-O1 answered B: C-V14 becomes "no `run_cap` in each", the dry run says nothing about a default, and
  R65 loses its clause on that note.

Unverified, and marked so where it is used:

- Whether the bounded pass counts a bare `---` as one document. 1.0's reader gives null for it (F18); C-V6
  records what the pass does.
- The bounded reader's refusal of a stream of no document is not yet pinned for each entity type; C-V11
  pins it for a task, an agent, a goal and a team, and runs 1.0's reader beside each.
- That 1.0 never writes a `config.yml` that is not a mapping. `orch init`, `write_raw` and `write_merged`
  were read and each writes a mapping; any other writer was not traced (F18).
- That no 1.0 writer leaves one identity twice in an index. The writers were read once, at v1.0.0, and not
  again for this text (F19).
- That an unset `run_cap` means no cap of the project's own, and that `run_cap` is the only setting whose
  1.0 default and 1.1 default differ in effect (T21-O1). Neither was traced in code.
- The compatibility tests of issue 146, which pin 1.0's side of several rows, reached `main` on 2026-11-22,
  after this text was first written. For an index they run 1.0's reader with a test type of one field and
  with no entity type. What the reader of a real directory does with a repeated identity (it uses the
  array where every entry is a complete entity, and rebuilds it where one is not) is read from the code
  and was run once, for a task and for a context entry, by a probe that is not in the repository (F19).
  No test in the repository pins it; X-V5 does, for each of the five directories that have an index.

## `orch db` commands

| Command | Daemon | Notes |
|---|---|---|
| `orch db status [--json]` | running | R72. Shows the log's backlog and the file's length apart (R126), and the age of the oldest read (R127). |
| `orch db verify [--quick] [--blobs]` | running | `integrity_check` (or `quick_check`), `foreign_key_check`, and: every lease names a run of its task with the same epoch; every epoch is below its task's `next_epoch`; no two rows of a table share a `position`; every `done` task has a landing state; every named blob has a `blobs` row; no two tasks of a project have one fold key, and the index that keeps it so is present (R117). |
| `orch db query '<statement>' [--limit n] [--json]` | running | One statement over the diagnostic views below (R74, R130); 1,000 rows by default, 100,000 at most; 16 MiB; 5 s. |
| `orch db backup` | running | Writes `backups/<UTC time>-g<generation>/`, created by the command, and takes no other destination (R75). The blob sweep waits while it runs. |
| `orch db prune [--older-than <days>] [--dry-run]` | running | R79. |
| `orch db export --to <file>` | running | The file must not exist; created 0600. Written in the export format of "Import bounds". |
| `orch db restore <backup> [--yes] [--stop-runs] [--assume-stopped <run id>]...` | stopped | R76 to R78, R100, R117, R132, R133, R138. `--stop-runs` stops only what the replaced state records; a process that only the backup records is named and never signalled. The replaced state goes to `backups/pre-restore-<UTC time>/`. A restore replaces no workspace: the project directory is not part of a backup, and a restored task finds its workspace by R118 or not at all. |
| `orch db restore --continue` or `--abandon` | stopped | R138. The only commands that run while the marker of an unfinished restore exists: the first finishes it, the second puts the replaced state back. |
| `orch db vacuum` | stopped | Needs free space of the database's size above the floor. |

Two things are called a backup and they differ (AP9). The copy before a migration is made by the daemon
itself, before every pending migration, checked, and holds the database alone (R124). A scheduled backup, a
job that runs `orch db backup` on a timer and rotates the results, is not in 1.1; the manual tells the owner
to run `orch db backup` when a full restorable point is wanted. A backup that the owner copies out of the data
directory has left the directory's protection (R136).

**How a restore replaces the state** (R138). A restore holds the directory's lock from its first step to its
last (kind `restore`), so no daemon and no other command runs beside it. The names below are proposals,
like every name that was added after the answer to O9, and so is the marker's bound of 4 KiB.

The state has seven **members**, always in this order and always by these names, which no input chooses:
`orch.db`, `orch.db-wal`, `orch.db-shm`, `blobs/`, `repos/`, `spool/` and `generation`. The *replaced*
members are those that stand at their live names when the marker is written; `-wal` and `-shm` are among
them only where they exist. The *staged* members are those that the staging directory holds: never a
`-wal` or a `-shm`, and the database alone when the copy that R124 makes is restored. A member can be
replaced and not staged, or staged and not replaced, and no rule below takes the absence of a member that
was never staged to mean that something was moved in.

1. *Checks, with nothing written.* The backup against its manifest (R76); the task ids and the workspaces
   of R117; the processes of R100; the list and the confirmation of R78 and R133; and free space: at least
   the size of the backup by its manifest (database, blobs and repositories) above the disk floor of B11,
   because the replaced state is kept and the backup is copied. A refusal ends here.
2. *Staging.* `restore-staging-<random hex>/` is created exclusively in the data directory, so that it
   lies on the filesystem of the live state. The backup's files are copied into it through the
   confined-directory module (R102), each checked against the manifest as it is written. The staged
   database is then opened and, in one transaction, given the new lineage and the new generation (R77):
   one more than the highest generation that the replaced directory's file, its database and the backup
   record. In the same transaction every lease it holds is ended and its run closed, as R100 says. The
   database is checked (`quick_check`, `foreign_key_check`), checkpointed and closed. The close is
   expected to leave no `-wal` and no `-shm`. That is looked at and not assumed: if the staging
   directory then holds a name that is not a staged member, the restore refuses here, exit 1, with no
   marker written and the live state untouched. The SHA-256 of the staged `orch.db` is taken. The file
   is opened once more and read to hold the new lineage, the new generation and no active lease, and is
   closed; the directory is looked at again, and the SHA-256 is taken again. A name that is left over, a
   value that differs from what was written, or a second hash that differs from the first is the same
   refusal. So the bytes that the hash names were read, with no log beside them, to hold what step 5
   must find, and step 5 needs no connection to find it. The staged `generation` file and an empty `spool/` are written, and every file
   and directory is synced. Then `backups/` is made if it is absent, `backups/pre-restore-<UTC time>/` is
   created exclusively, 0700 and empty, and that directory, `backups/` and the data directory are synced.
3. *Marker.* The marker holds a format number, the random part of the staging directory's name, the UTC
   time that names `backups/pre-restore-<UTC time>/`, the backup's name and the hash of its manifest, the
   new lineage and generation, the confirmation as it was given, the direction, `forward`, the SHA-256 of the staged `orch.db`
   from step 2, and for each of the seven members whether it is replaced and whether it is staged. It is at most 4 KiB. It is never
   written in place. Each version is written to `restore-in-progress.tmp-<random hex>`, created
   exclusively, 0600, in the data directory; that file is synced; it is renamed onto
   `restore-in-progress`; and the data directory is synced. So the marker is absent or is one whole
   version, and a stop leaves no part of one. A command that reads it checks every field and opens no
   path taken from it: the two directory names are built again from the checked random part and time,
   and the members are the seven fixed names (R101, R102).
4. *Swap,* in two phases, of which the second never begins before the first has ended. *Out:* each
   replaced member, in the order above, is renamed from its live name into the `pre-restore` directory;
   after that no member stands at a live name. *In:* each staged member, in the order above, is renamed
   from the staging directory to its live name. After every rename both directories that it touched are
   synced: the one the member left and the one it entered.
5. *End.* The final layout is checked: every staged member stands at its live name and the staging
   directory is empty; every replaced member lies in the `pre-restore` directory; and no `orch.db-wal` or
   `orch.db-shm` stands at a live name. The database is then checked without a connection: the bytes of
   `orch.db` at its live name, read as R102 says, hash to the SHA-256 that the marker holds, so they are
   the bytes that step 2 read to hold the marker's lineage and generation and no active lease; and the
   `generation` file holds the marker's generation. SQLite does not open the database at its live name
   while the marker stands: not in this step, not in a continuation and not in an abandonment. So the
   check makes no file, a stop inside it leaves nothing behind, and it is run again from its start. The
   first connection to the new database is the next start's, after the marker is gone. No way of
   opening a database that is said to leave no `-wal` or `-shm` is relied on for this. Only then is the
   empty staging
   directory removed and the data directory synced, and after that the marker is removed and the
   directory synced. A check that fails leaves the marker and everything else as it is, and the command
   refuses, exit 1, and names what it found. The restore reports the new generation and lineage, the
   leases it ended and where the replaced state lies.

| A stop in | Leaves | Then |
|---|---|---|
| Step 1 | Nothing new | Nothing to do |
| Step 2, or a failed copy or a full disk there | The live state untouched; a staging directory and perhaps an empty `pre-restore` directory, with no marker | The next command or start removes both, unread, and counts them. The restore is run again from its start |
| Step 3, before the marker's rename | The same, and a temporary marker file | The same: the temporary file is removed with them, unread |
| From the marker's rename to its removal | The marker | Nothing starts: the daemon and every offline command refuse, exit 1, and name the marker and the two commands below |

*What a continuation reads.* `orch db restore --continue` and `orch db restore --abandon` read the marker
and then look where each of the seven members lies: at its live name, in the staging directory, in the
`pre-restore` directory. A temporary marker file beside the marker is removed first, unread; the marker
that stands decides. These places are expected, and no others:

| The marker says the member is | It may lie | Which means |
|---|---|---|
| Replaced and staged | At the live name and in staging | Not yet moved out; or, once the direction is `back`, put back |
| | In staging and in `pre-restore` | Moved out, not moved in |
| | At the live name and in `pre-restore` | Moved in |
| Replaced only (a `-wal` or a `-shm`, say) | At the live name | Not yet moved out |
| | In `pre-restore` | Moved out |
| Staged only | In staging | Not moved in |
| | At the live name | Moved in |
| Neither | Nowhere | Nothing to do |

For a member that is replaced and staged, what stands at the live name is the replaced member while the
`pre-restore` directory does not hold that member, and the staged member once it does; where the staged
member lies never decides that alone. Across members one more thing must hold: no member is moved in
while any replaced member is not yet moved out. Everything else is unexpected: a member in three places,
or in none where the marker expects one; a `-wal` or a `-shm` at a live name beside a member that is
moved in; a member that the marker calls neither, standing anywhere; a name in the staging or the
`pre-restore` directory that is not one of the seven. On an unexpected layout the command changes
nothing, leaves the marker, refuses, exit 1, and names what it found.

Two cases are decided before the table above is applied, in this order. First, under the direction
`back`: whether the abandonment has reached its end (below). Second, under the direction `forward`:
whether the staging directory is gone. The rule for a staging directory that is gone belongs to
`forward` alone and is never applied under `back`.

`orch db restore --continue` runs only while the direction is `forward`. It first finishes the phase
*out*, for every replaced member that is not yet moved out, and only then moves in what is not yet moved
in; then it does step 5 with its checks. It asks nothing again, and its result is that of a restore that
was never stopped. Once the staging directory has been removed under `forward` the restore is complete
but for the marker: `--continue` then checks the final layout as step 5 does and removes the marker, and
does nothing else.

`orch db restore --abandon` reads the marker's direction first.

*While the direction is `forward`* it runs only while the staging directory exists; once that directory
is gone the command is refused and says that the restore is complete and that `--continue` removes the
marker. Otherwise it first publishes a new version of the marker with the direction `back`, by the four
actions of step 3. Then, in the reverse order of the members, it moves every member that was moved in
back into the staging directory, and then every replaced member from the `pre-restore` directory back to
its live name, with both directories synced after each rename. It checks that every replaced member
stands at its live name, that no other member does, and that the `pre-restore` directory is empty. It
ends with its *cleanup*, in this order: it removes the staging directory with what it holds, then the
empty `pre-restore` directory, then the marker, and syncs the directory that held each.

*While the direction is `back`* an abandonment was begun and was stopped, and the command first asks
whether it had **reached its end**: every member that the marker calls replaced stands at its live name;
no member that the marker does not call replaced stands at a live name; and the `pre-restore` directory
is empty or is absent. This is asked before the table above is applied and before anything is asked of
the staging directory. When it holds, the replaced state is whole and only the cleanup is left, and the
command does the cleanup and nothing else. What is left of the staging directory, whether that is all of
it, a part of it or nothing, is scratch and is removed unread, as a staging directory with no marker is;
the `pre-restore` directory is removed if it is there; the marker is removed last. Each of the three is
skipped when an earlier run already did it, so the command can be stopped and run again at any point of
the cleanup. A member that is replaced and staged and stands at its live name alone, its staged copy
already removed, is what a stopped cleanup leaves, and here it is expected. The test is sound because a
staged member is moved in only after every replaced member lies in the `pre-restore` directory: while
that directory is empty or absent, nothing at a live name is a staged member. When the test does not
hold, the command goes on by the table above: it moves back what is still moved in, puts back what is
still in the `pre-restore` directory, makes the three checks and does the cleanup. A staging directory
that is gone while a replaced member is not at its live name is a layout that no step leaves, and it is
refused with the marker kept.

Its result is the replaced state byte for byte, with its lineage, its leases, its log and its spool as
they were. Once `back` is published `--continue` is refused, so a direction never changes halfway. A stop
during the change of direction leaves `forward` or `back` whole; while it is still `forward` no member
has been moved back, so either command may still be run.

A marker that fails its check (an unknown format number, a field outside its bounds, more than 4 KiB, a
list of members that no restore can have, a name that matches no directory) fails closed. It is still a
marker: nothing starts, and both commands refuse, exit 1, change nothing and name the manual recovery.
Publishing a marker as step 3 does cannot leave such a file after a stop of the process. After a loss of
power it can, where a sync had not reached the medium ("What a sync promises"). Otherwise one that fails
its check was damaged or was not written by this program. In every case no command removes or repairs
it. The RUNBOOK is to give
the manual recovery: the owner compares the live names, the staging directory and the `pre-restore`
directory, puts one whole state at the live names by hand, and removes the marker. That text is a duty of
the documentation task (T26) and is not written yet. With no marker at all, either command changes
nothing and says that no restore is unfinished. Either command can be stopped at any point and run again,
and a rename or a sync that fails stops it with the marker in place.

Stops that the rules above must give, worked out and not run. In each the replaced state has `orch.db`,
`orch.db-wal` and `orch.db-shm`, and the staged database has neither companion:

| The stop | What lies where | `--continue` | `--abandon` |
|---|---|---|---|
| After the temporary marker is created and before it is written, or before its rename | No marker; the live state whole | Says no restore is unfinished; the scratch is removed | The same |
| After `orch.db` is moved out and before `orch.db-wal` | Database: staging and `pre-restore`. `-wal` and `-shm`: live names | Moves `-wal` and `-shm` out, then the other replaced members, and only then moves in. No old `-wal` is left beside the new database | Puts `orch.db` back beside its own `-wal` and `-shm` |
| After `-wal` is moved out and before `-shm` | `-shm` alone at a live name | Moves `-shm` out first, then goes on | Puts `-wal` and the database back |
| After every member is out and before any is in | Nothing at a live name | Moves in | Puts every replaced member back |
| After `orch.db` is moved in and before `blobs/` | Database: live name and `pre-restore`. `blobs/`: staging and `pre-restore` | Moves in the rest | Moves the new database back to staging, then puts every replaced member back |
| After everything is in and before the final check | Step 5 not done | Checks, then ends | Moves everything back |
| Inside the final check: after the layout is read, while the bytes of the database are read | The `forward` marker, the complete new state and the empty staging directory. The check has made no file | Runs the check again from its start, then ends | Moves everything back |
| After the staging directory is removed and before the marker, the direction being `forward` | The `forward` marker and the complete new state | Checks the layout and removes the marker | Refused: the restore is complete |
| During the change to `back`, before the rename | The `forward` marker and a temporary file | Runs as before | Publishes `back` again and goes on |
| `--abandon` is stopped after `back` and after some members are back | The `back` marker | Refused | Finishes |
| `--abandon` is stopped while it removes the staging directory, a part of it gone | The `back` marker; every replaced member at its live name; `pre-restore` empty; a part of the staging directory | Refused | Finds the abandonment at its end: removes the rest of the staging directory unread, then the `pre-restore` directory and the marker |
| `--abandon` is stopped after the staging directory is removed and before the `pre-restore` directory | The `back` marker; the replaced state whole; no staging directory; `pre-restore` empty | Refused | The same end: removes the `pre-restore` directory and the marker. It is not refused, and nothing says that a restore is complete |
| `--abandon` is stopped after the `pre-restore` directory is removed and before the marker | The `back` marker; the replaced state whole; neither directory | Refused | Removes the marker |
| The direction is `back`, the staging directory is gone, and a replaced member is not at its live name | Unexpected: no step leaves it | Refused | Refused; the marker stays |
| Either command is run a second time after it has ended | No marker | Says no restore is unfinished | The same |
| An `orch.db-wal` stands at a live name beside a database that is moved in | Unexpected | Refused; the marker stays | Refused; the marker stays |
| The marker is empty or fails its check | Any | Refused; nothing changes | Refused; nothing changes |

So no start meets a database of one state beside the blobs, the repositories or the log of the other, or a
restored database whose leases are still active. This staging decides nothing about a workspace that already
stands at a restored task's name: that is the fifth point of AP14, it is the owner's, and T23 builds no
handling of it before the ruling.

**Diagnostic views** (R74, R130). `orch db query` reads views named `diag_<table>` and nothing else. There is
one for each table of "Schema for 1.1" outside these: `identities`, `identity_tokens`, `daemon_config` and
`request_keys` have none; `diag_project_config` leaves out the value of `legacy.config`; `diag_blobs` shows a
blob's hash and size and never its bytes; `diag_schema_migrations` is the one view of the daemon-wide group.
The views and their columns are listed in `docs/storage.md`, and a test compares that list with the schema, so
that a new table or column is in no view until someone lists it. The functions a statement may call:
`count`, `sum`, `total`, `avg`, `min`, `max`, `group_concat`, `length`, `substr`, `instr`, `lower`, `upper`,
`trim`, `ltrim`, `rtrim`, `replace`, `coalesce`, `ifnull`, `nullif`, `abs`, `round`, `typeof`, `hex`, `quote`,
`like`, `glob`, `date`, `time`, `datetime`, `julianday`, `strftime`, `unixepoch`, `json_extract`, `json_type`,
`json_array_length`, `json_valid`, `row_number`, `rank`, `lag`, `lead`. Every other function is refused. The
authorizer is the control; no check reads the statement's text.

## Attention queue, disk floor, usage

- **Attention.** Kinds in 1.1: `disk_floor` (blocking, daemon-wide) and `dispatch_off_with_work` (warning, per
  project). `orch attention list|show|ack`; `orch status` prints the number of open items. The daemon opens
  and resolves items; the owner can only acknowledge, which records who and when and resolves nothing.
  Neither kind holds text from an agent: an item's words are fixed for its kind, with values the daemon
  derived, and so is the remedy it shows (R131). A failed writer is not an item, because writing one needs
  the writer; `orch status` and `orch doctor` report it from memory (R128).
- **Disk floor** (R85). Free space is what the filesystem offers an unprivileged process. It is read every
  30 s and before a claim, an import, a backup, an export and a vacuum. "Operations that add work" are
  `task.create`, `goal.create`, attachment staging, `db.backup`, `db.export` and `orch import`.
- **Usage** (R86, R87). The agent CLI's version is probed once per adapter per daemon generation and cached.
  The model is the one the adapter was asked to use, replaced by the one the CLI reports when it reports one.
  1.0's per-agent total stays as `agents.tokens_used`.

## Maintenance jobs

One timer; each job has a period and a stored due time (R43). All run on lane 4 except lease expiry and the
checkpoint, which has a connection of its own and waits in no lane (R126).

| Job | Period | Does |
|---|---|---|
| `leases.expire` | 10 s | Renews supervised runs, then expires what is left past its time (lane 1). |
| `intents.perform` | at start, then 30 s | Handles pending intents; deletes done ones after 7 days. |
| `attention.evaluate` | 30 s | Disk floor and dispatch-off-with-work. |
| `request_keys.expire` | 5 min | R47. |
| `context.expire`, `messages.expire` | as in 1.0 | 1.0's expiry steps, now jobs. |
| `wal.checkpoint` | 5 min after its last completed checkpoint, or when the log's backlog reaches 64 MiB, looked at once a second | Passive checkpoint on its own connection, never on the writer's (R126); over 256 MiB of backlog it first interrupts pooled reads older than 30 s (R127). The backlog is the frames not yet checkpointed, never the file's length. Each call is recorded with what started it and its outcome, and only a call that succeeded moves the timer (R137); calls that keep failing, and a backlog that stays over its ceiling, are reported (R140). At a stop no new call starts (R139). |
| `events.prune` | 24 h | R62. |
| `blobs.sweep` | 24 h | R60. |
| `attention.expire` | 24 h | R84. |
| `db.optimize` | 24 h | `PRAGMA optimize`. |

## orch-peer

What breaks, exactly (B15), at `fb6a1e1`:

| Place | What it does today | In 1.1 |
|---|---|---|
| `crates/orch-peer/src/worker.rs`, `list_tasks` (near line 805) | Reads `tasks/_index.json` to predict an index rebuild. | The read is removed; there is no index to rebuild. |
| `crates/orch-peer/src/lib.rs` (near lines 299 to 316) | Reads `messages/_index.json` and builds a `FileMessageStore` to reject a remote message. | Calls the message service through the port. |
| `crates/orch-peer/src/worker.rs` (near line 868) and its other calls on `engine.tasks`, `engine.agents`, `engine.messages`, `engine.state` | Use the 1.0 store traits. | Unchanged source: `orch-engine` keeps a facade with those shapes over port v2 for `orch-peer` alone, removed with it in 2.0. |
| `orch-peer`'s tests that write index and state files | Arrange 1.0 files by hand. | Rewritten to arrange through the facade. |
| `remotes.yml`, `remotes/`, `peer-id` | orch-peer's own files under `.orchestry/`. | Unchanged, and therefore not in the database, a backup or an export. |

The peer wire protocol does not change, so a 1.0 peer and a 1.1 daemon are expected to interoperate; R89's
recorded check proves it. The owner approved the first two rows (AP1); the check on a second host is still due.

## Non-goals

Everything in X. Also: scheduled backups; a paged dashboard (the TUI and `orch web` still fetch every page);
moving agent workspaces out of the project directory; a usage report command (`orch db query` serves until one
is asked for); a metrics endpoint other than `metrics.get`; deleting 1.0 files; any change to the global
configuration file or to organization templates, which stay files. Since the owner's decision of
2026-11-21 (record I of "Owner's answers"): a claim-latency budget, a measurement of event throughput or of
latency as a condition of anything, performance tuning, and the event fallback.

## Constraints

- Linux is the platform of development, test and qualification through 2.0 (record K); the pinned toolchain; one project per daemon; a local disk. Product code that already handles macOS is not removed, and macOS compatibility work follows 2.0.
- The frozen handshake, the stop intent, the no-daemon contract and exit 7 of ADR 0012 are unchanged.
- CLI and `--json` output shapes of 1.0 commands are unchanged apart from the added `revision` field (R48,
  R49). The peer protocol is unchanged.
- 1.1 is a breaking release for stored state only: one import per project.
- "64 KB" in B7 is read as 64 KiB (65,536 bytes) everywhere in this spec.

Security checklist:
- Default bind and exposure: the Unix socket in `run/`, 0600 in a 0700 directory, same-uid check as in 1.0.
  Nothing listens on a network. `orch db` and `orch import` have no remote form.
- Bounds on input size, count and rate: the frame and admission limits of protocol 2 stand. Added: 4 read
  connections; list limit 1,000; key 128 characters; cursor 512 bytes; kept result 64 KiB; `db.query` 100,000 rows and 5 s; the
  budgets of "Import bounds" on every 1.0 document (bytes, depth, nodes, aliases, expansion, tags), event
  line, listing and export file, each checked before anything is built from the input (R109 to R116);
  `.orch-project` 64 bytes; `legacy.config` 64 KiB.
- Permissions of files it writes: the data directory 0700 and files 0600; backups and exports 0600;
  `.orch-project` 0644 (it holds an id, no secret).
- Secrets and environment passed to children: unchanged. `identity_tokens` holds hashes only. `db.query`
  reads the diagnostic views alone, which show no identity, token, request result or daemon setting (R74); it
  is still owner-only and absent from MCP.
- Untrusted text rendered to a terminal or passed to a shell: `db.query` output, the dry-run report and holder
  records are escaped as 1.0 escapes diagnostics; an attention item holds no agent text in 1.1 (R131).
  `db.query` takes SQL from the owner only and runs it on its own read-only connection under an authorizer
  that denies by default (R130). Source
  files of an import, an export file and a backup's manifest are untrusted input: bounded, never executed,
  and no name, id or path in them selects a file below the data directory ("Filesystem names", R101 to R108,
  R117, R118).
  An export file is opened as a hostile SQLite file: read-only and immutable, schema untrusted, no trigger,
  view, function or extension, its schema compared and never adopted, and nothing published until all of it
  has passed ("Import bounds", R113 to R115).
  An import signals no process (R105). In the project directory only a checked task id names a workspace and
  only a branch of the derived shape is merged or deleted (R106, AP14). No two tasks of a project have ids
  that differ only in case, on any platform, and a directory or a branch is reused, removed, merged or
  deleted only when a listing holds it under exactly the task's name, so two ids never share a workspace and
  one task's cleanup never removes another's (R117, R118). A backup's process records make a restore
  refuse and report, and never signal: `--stop-runs` stops only what the replaced state of this directory
  records (R100). Whether a backup may be taken as this project's own state, for a workspace that already
  stands at a task's exact name, is a separate question and is open (the fifth point of AP14).
- Same-uid limit, stated plainly: any process of the owner's user can open the socket or the database file.
  The matrix is not a defence against such a process in 1.1; it becomes one with authentication in 1.4.

## Verification environment

- Linux only, through 2.0 (record K of "Owner's answers"): GitHub's Linux CI and the Linux development
  hosts run every test named above. No row of this spec waits for a macOS result.
- Native Linux hardware, by the performance engineer: R5 and R63. Deferred on 2026-11-21 with
  the measurement (record I of "Owner's answers"): not due in 1.1. The macOS hardware that R5 and R63 once also
  named is outside the scope (record K).
- A real suspend and resume of an isolated Linux host with a supervised run in flight (R42; issue 110; D6): kept,
  pending, not run. The clock-seam test of R42 and the prototype's elapsed-time model prove renewal ordering and
  the deadline arithmetic on an injected clock, and do not stand for it. Nobody suspends a host without the
  owner's permission for that host. The macOS laptop that stood here for R6 and the recorded part of R42 is
  withdrawn (record K); a physical macOS observation is deferred to the After 2.0 milestone (issue 178).
- The owner's machine and permission, on a copy outside the repository: R71.
- A second Linux host for the peer check: R89.
- A filesystem that folds case is not required and none is provisioned (record K). R118 is accepted on Linux
  through three paths: the injected folding listing; the native exact-name checks (the directory opened by that
  one name, listed under it byte for byte, the same device and inode) with their canaries, which run on
  whatever Linux filesystem the test uses; and the real folding branch, which runs only where the test's
  temporary directory does fold case. Each test prints which paths it ran. A path that did not run is reported
  as not run, with the reason (the filesystem keeps case, or the test is not implemented yet); it is never read
  as a pass and is not `env-limited`. R117's tests need no folding filesystem.
- Not needed: a browser, a native terminal profile, tmux (no dashboard behaviour changes).

The owner's answer D6 (2026-11-20) allows four things: a real suspend and resume on an isolated macOS
machine (R42), evidence from native Linux hardware (R5, R63), the peer check on a second host (R89), and a
copy of the live project outside the repository for R71 later, with no private content in the repository or
in a PR. The answer gives access and proves nothing: each of those checks was due. Record K (2026-11-23)
withdraws the macOS machine of the first: no macOS suspend is required before 2.0. The check itself stays, on an
isolated Linux host (issue 110), and the owner's permission to suspend that host is still to be given: D6 named a
macOS machine and is not read as that permission. The peer check stays, on a second Linux host;
the evidence from native Linux hardware stays deferred with R5 and R63; and the copy for R71 stays under the
owner's permission, with the same limits on private content. No native Linux host
has been named yet. The first run's Linux figures come from a container on the macOS machine, which is a
virtual machine on that hardware and never stands for the host. The answer names the copy for R71. Using it
for the cascade count of plan task T4c is covered neither by its words nor by the answer to AP8, whose
recommendation named no source for the count, so that use still needs the owner's word. Since
2026-11-21 the evidence from native Linux hardware for R5 and R63 is not due in 1.1
(record I of "Owner's answers"); the other three checks are unchanged.

## Open questions

Each goal open point with a recommendation, then the points this spec and its reviews found, then the
eight decisions that the approval request of 2026-11-20 asked beside them (D1 to D8). The owner has answered
all 38 ids: 37 as recommended, and AP13 in its direction, with its numbers and results still the owner's to
accept. On 2026-11-21 the owner deferred the measurement of AP13, and with it those numbers and results
(record I). Five points were added on 2026-11-22 and are not answered: they have a table of their own,
"Points raised on 2026-11-22 and not answered", and record J. "Owner's answers" below the tables has one
row for each id and says what each answer does not
settle. An answer is the owner's word on a recommendation. It is not a proof, a review or a measured
result, and neither a reviewer's nor the lead's agreement stands in for one. Where an answered row
recommended nothing, as the fifth point of AP14 did, the answer chooses nothing there.

| # | Point | Recommendation | Main alternative |
|---|---|---|---|
| O1 | How a client finds the daemon and its data directory | **Decided by the owner, 2026-11-20: as recommended.** Introduce `.orch-project` in 1.1 (R34), keep the 1.0 search order, name the data directory from the project directory when it is created and find it by project id afterwards (R33). `--project` and `ORCH_PROJECT` arrive in 1.5, when a project can exist without a directory. Reason: the import is the one moment every project is touched, and without the pointer a moved project directory looks unimported and invites a second import of stale files. The answer's condition on copies and worktrees is R34 and R135, and on a pointer write that is interrupted, R125. | No pointer in 1.1: `.orchestry/` stays the marker and the data directory is found by the derived name alone. Less change now, the move hazard stays, and the pointer arrives in a later breaking step. |
| O2 | Which tables 1.1 creates | **Decided by the owner, 2026-11-21: as recommended.** The 33 tables and the eight differences in "Schema for 1.1". | Create all 50 now: no later table migrations, but 17 untested empty tables and constraints nobody has exercised. |
| O3 | Import with the daemon stopped; how long 1.0 files are kept | **Decided by the owner, 2026-11-20: as recommended**, in the three parts the approval request put: stopped, refused while 1.0 work may survive, and 1.0 files kept. The acceptance of a 1.0 state that changed afterwards is AP16, answered as recommended on 2026-11-21. Stopped, both daemons (R68), and refused while a 1.0 run may still be at work on this machine or on a peer (R119). After the import a changed 1.0 state stops the next start (R121, AP16). orch never deletes 1.0 files; the manual advises keeping them until release 1.2 is running and one backup has been restored in a test, and lists the 1.0 state files that may then be removed by hand. The directory `.orchestry/` itself, its `workspaces/` and the peer files stay in use and are never on that list. | Import into a running 1.1 daemon: no stop, but the import shares the writer with live work and its resume is harder to reason about. |
| O4 | Dispatch at start | **Decided by the owner, 2026-11-21: as recommended.** As it was when the daemon stopped; off after import, restore and a migrating start; the flag overrides (R44). The `dispatch_off_with_work` item covers a forgotten "off". | Always off, as 1.0: simplest, and every restart needs a second command. |
| O5 | Maintenance loop | **Decided by the owner, 2026-11-21: as recommended.** One timer with a due time per job (R43): one place for order, shutdown and the lane, and due times that survive a restart. | One timer per job: independent, but eleven tasks to stop and no shared order. |
| O6 | Resolved attention items; doctor | **Decided by the owner, 2026-11-21: as recommended.** Keep 90 days. A blocking item makes `orch doctor` exit 1; a warning does not (R84). Doctor's exit for its existing checks does not change. | Doctor stays exit 0 and only prints: no script breaks, and a blocked daemon looks healthy to one. |
| O7 | Unknown config keys; `max_concurrent_agents` | **Decided by the owner, 2026-11-21: as recommended.** As proposed (R70), with a hard stop over 64 KiB. What a key that the file does not set imports as was no part of the recommendation or of the answer: T21-O1. | Refuse an import with any unknown key: strict, and blocks projects that carry extension keys. |
| O8 | Request-key expiry | **Decided by the owner, 2026-11-21: as recommended.** Ships in 1.1 as `request_keys.expire` (R47). | None; the goal requires it. |
| O9 | Names | **Decided by the owner, 2026-11-21: as recommended**, the set holding the names that accepted review corrections added. Approve the names in this spec as a set: operations in "Protocol 3 and errors", commands in "`orch db` commands" and "Import rules", settings `storage.synchronous`, `retention.events.success_days`, `retention.events.failed_days`, `attention.resolved_retention_days`, `run_cap`, `preapproval`, `legacy.config`; variables `ORCH_DATA_DIR`, `ORCH_DATA_HOME`, `ORCH_REQUEST_KEY`; metrics `scheduler_tick_seconds`, `scheduler_open_tasks`, `writer_wait_seconds`, `writer_txn_seconds`, `writer_txn_over_bound_total`, `writer_busy_timeouts_total`, `events_dropped_total`, `disk_free_bytes`, `disk_floor_bytes`, `writer_commit_seconds`, `wal_bytes`; and the names that the review corrections added: the options `--assume-stopped` and `--accept-legacy-change`, the error codes `storage_unavailable` and `snapshot_expired`, the views `diag_<table>`, and the report classes `in_flight_live`, `in_flight_remote`, `index_differs` and `index_unreadable`; and with the closure corrections the metric `wal_file_bytes`, the `daemon.status` count of unspooled finishes, and the method name `ap13-realistic-v1`. The report classes `index_entry` and `index_duplicate` were added on 2026-11-22 and are not in the set the owner answered: they are proposals. | Rename any of them at the gate; a rename after the protocol PR costs a protocol bump. |
| O10 | Fixtures without private data | **Decided by the owner, 2026-11-21: as recommended.** The conformance corpus (`fixtures/storage-v1/`, entry step B) is small, written by hand and invented. The importer's corpora are generated (R90): a seeded generator drives the 1.0 file backend through scripted scenarios, so the format is real and the content invented; its sizes follow counts and percentiles taken from the live copy, which are numbers; a hand-written corpus covers damaged input. The live copy is used only for R71 and R5, outside the repository (R5 was deferred on 2026-11-21: record I). | Scrub a real project: any scrubber can miss something, and the result cannot be regenerated. |
| AP1 | orch-peer cannot stay byte-for-byte unchanged (F7) | **Decided by the owner, 2026-11-20: as recommended.** A second host must prove it (R89). Approve the two source changes in "orch-peer"; the wire protocol and peer files stay. | Turn remote peers off in 1.1 until their removal in 2.0: no change to `orch-peer`, and remote agents stop working for four releases. |
| AP2 | The catalogue has 111 operations, the goal says 115 (F1) | **Decided by the owner, 2026-11-20: as recommended.** Treat the catalogue's own class test as the authority; 1.1 adds 12 operations. | The owner names the four operations the goal counted, if they are meant to exist. |
| AP3 | Whether an agent's session becomes a task principal in 1.1 | **Decided by the owner, 2026-11-21: as recommended.** No: it stays an owner session for the 1.0 catalogue, so no agent workflow changes in the storage release; the new owner-only operations are refused in agent context (R54). The task column becomes real with authentication. | Enforce the task column now: safer on paper, but same-uid makes it advisory, and it changes what agents can do today. |
| AP4 | CLI lists | **Decided by the owner, 2026-11-21: as recommended.** Return all rows in the 1.0 shape by default; `--limit` and `--cursor` opt in to one page (R49). Scripts and agent prompts that parse `--json` keep working. | Page by default: honest about cost, breaks every consumer of the current output. |
| AP5 | `preapproval` for a project created by `orch init` in 1.1 | **Decided by the owner, 2026-11-20: as recommended.** Turned on only in the owner's own context (R133). On, as 1.0's default policy is `auto`; `orch init` prints the trust statement. Revisit in 1.5, when the daemon sets the verdict. | Off: safer default, and every new project stops at `review` until the owner turns it on. |
| AP6 | What 1.1 writes under `.orchestry/` | **Decided by the owner, 2026-11-21: as recommended.** Only `workspaces/`, orch-peer's files and, at `orch init`, the authored files `workspace-exclude` and `.gitignore`, as today; never a 1.0 state file. B2 binds the importer; this states the same for the daemon. | Move workspaces into the data directory: `.orchestry/` is then never written, but every agent CLI sees new paths and may ask for trust again. |
| AP7 | "Over the local socket only" (B8) for commands that need the daemon stopped | **Decided by the owner, 2026-11-20: as recommended.** Read it as "owner, local, never remote": import, restore and vacuum run offline under the directory lock; the other `orch db` commands use the socket. | Run all of them through a daemon in a maintenance mode: one path, and a daemon that must swap its own database while serving. |
| AP8 | An atomic operation larger than 1,000 rows | **Decided by the owner, 2026-11-21: as recommended.** It runs whole and is counted (R27); lane waits are budgeted only while that count is zero. No measurement of the largest cascade exists: the first draft cited one in deliverable D, and D holds none. Plan task T4c counts the rows of the largest goal pause and failure cascade on the live copy, counts only, before the engine PR that builds them; the owner's answer D6 names the copy for R71 and not for this count, and the recommendation the owner accepted for this point named no source for it, so this use of the copy still needs the owner's word. | Cut cascades into steps: keeps the bound, exposes half-finished cascades to readers. |
| AP9 | Backups in 1.1: a scheduled job, and the copy before a migration | **Decided by the owner, 2026-11-21: as recommended.** Two answers. No scheduled backup job in 1.1; one is a small follow-up once restore has field use. And the daemon itself copies the database before every pending migration, checks the copy and keeps the newest two (R124): the library report requires a backup before a migration, and an owner who never needed a backup command will not have taken one. | Add a daily `db.backup` job with rotation now. Or leave the copy before a migration to the owner, as the first draft did: nothing to build, and a migration that commits and is wrong can then be undone only from the 1.0 files of import day. |
| AP10 | The 100-entry retry cap goes away (R40) | **Decided by the owner, 2026-11-21: as recommended.** Accept: a table needs no cap, and 1.0's drop silently stranded a task. | Keep the cap for exact parity. |
| AP11 | What fences a run across a restore. The lease prototype's first finding: a restored snapshot has lost the epochs issued after the backup, so "largest stored epoch plus one" can issue a number twice. | **Decided by the owner, 2026-11-20:** a random restore lineage beside the epoch counter, and gate line 9 amended to "no lineage and counter pair is reused" (R77, R132). The generation floor that took epochs from a clock is withdrawn: on a directory with no earlier state it could issue a number twice, as the security review's arithmetic showed, so no promise about a number rests on it or on anything else here. A restore still ends every restored lease and refuses while a recorded process lives (R100), and dispatch is off after it (R44). The lease prototype's report (R6) is still to confirm the mechanics. | Not chosen: a high-water mark kept outside the data directory, with the restore refused when it is missing; or the clock-derived floor alone. |
| AP12 | What a suspend does to a run in 1.1. The lease prototype measures a holder's deadline on time that counts a suspend, so that a holder that slept past it stops. Its review notes that in 1.1, where the daemon and the agent sleep together, that rule ends every run in flight after any sleep longer than the deadline (90 s in the prototype). | **Decided by the owner, 2026-11-20: as recommended.** The proof on a real suspend was due (R42, D6); record K withdraws its macOS form and moves it to an isolated, authorized Linux host (issue 110), where it is pending. Keep R42: in 1.1 a suspend ends no run. The one daemon is the only grantor and it slept too, so no other holder can exist; on resume it renews what it still supervises before it expires anything. The holder's own deadline arrives with workers in 1.5, where the daemon can stay awake while a worker sleeps. R42's recorded check was a real suspend on an isolated macOS machine, and record K (2026-11-23) withdraws that machine; the check stays, for Linux. Not part of the owner's answer, and drawn by this spec from it: since no 1.1 holder stops itself, a waiting period after a restore proves nothing, and R100's refusal while a recorded process lives is the only ground for sending a restored task out again (the review of the prototype's corrections, N1). | Apply the 1.5 rule now: one rule in every release, and every lid close longer than the deadline loses the runs in flight, which 1.0 does not do. |
| AP13 | The measurement missed the budget on macOS. As deliverable D reports it: in a Linux container on the same machine every line was met; on macOS, 24 runs at ten times the nominal rate gave a 99th percentile of 1.9 s, the nominal rate 53.5 ms with a worst case of 170 ms, and no events 10.9 ms; the slowest event transaction took 592 ms for at most 12 rows, and a claim's own transaction up to 499 ms. No figure from native Linux exists yet, and R5 needs one. | **Deferred by the owner on 2026-11-21 (record I of "Owner's answers").** In 1.1 there is no measurement, no budget and no selection of a path: events are stored in SQLite by the batched design (R63) and the fallback is not built. What follows is kept as a record. The owner's direction, ruling and answers in it are not withdrawn and have nothing to act on until latency is taken up again, and nothing that it names as held, open or proposed is due in 1.1. **Direction decided by the owner, 2026-11-20, and kept by the answers of 2026-11-21; every number and every result is still the owner's to accept.** The owner chose to revise the workload and the budget and to measure anew, and not the diagnosis at the first budget that this row recommended until then. The owner approved changing the budget in principle and set no numbers; no relaxed budget is put in place of the first one silently. What follows from the direction: (1) the proposal is "Event measurement", with a claim a second, 1 to 24 event streams, a burst cell, 200 ms at the 99th percentile and 1,000 ms at the worst, and the production checkpoint inside every repetition with its claims reported apart (red team W2-R1); (2) every figure of the first run is kept and its FAIL stands (R5, R63); (3) the measurement runs on macOS and on native Linux hardware, and the proposed numbers come back to the owner with the results; (4) as first directed, if the revised budget is missed the fallback applies. The owner's ruling of 2026-11-21 amends that: every miss is kept; the fallback is selected only for a miss that is attributable to events; a miss without events (claims alone, C0, a burst alone) or on one platform alone selects no event path and goes back to the owner ("Event measurement", "What a miss selects"). Two answers of 2026-11-21 followed (record F): "one platform alone" is judged by kind of limit and population and not by cell, so two different kinds of miss in one cell, one on each platform, select no path and go back to the owner; and after a data set in which a mandatory repetition that is `valid` or `host_not_idle` has missed, no further data set is started without the owner's word, whatever the headline, the fix of a defect included ("The verdict"). Four answers of 2026-11-21 were given under the owner's delegation (record H): the five kinds (the regular claims, the claims of a checkpoint window, the claims of a burst window, an event's age, and a refusal or something still queued) are adopted and are operative, with the cell and the population kept in the match; a miss of steps 2 to 4 of "What a miss selects" anywhere in a data set keeps it from selecting an event path, while an eligible data set in which every miss is a valid miss in a population that holds events, shown by both platforms in the same cell, population and kind, still selects the fallback; and starting the second platform's block after a mandatory miss on the first, and replacing an `invalid` or `host_not_idle` set of five while such a miss stands, are each further measurement that needs the owner's word, with the old set and every miss kept. Selecting a path is not permission to measure it and does not qualify it to ship. A fallback that is selected is measured as built before anything ships on it, and that measurement is started only on the owner's word; no figure shows that it is faster. Settled by no answer, and open: what is done with a repetition or a block that is already running when a mandatory miss is seen, and whether the blocks of the two platforms may overlap or be run in turns; no measurement campaign is started before those two are settled. Open as well: whether a block that the owner allows after a miss completes the data set that holds the miss or is a data set of its own. This spec proposes no rule for stopping, scheduling or tearing down a block and none for which data set a block belongs to. Held by the owner until the results exist: the numbers of the budget; how lane 1's 50 ms, lane 3's 1 s and the shipped collector's 256 events or 250 ms sit beside the proposal, for each of which "Event measurement" states what this spec proposes; the idle-host limits, the platform classes, the eligibility rule and the two things under "What a miss selects" that are still proposals (what "a burst alone" is, and the step taken from "The host"), all of which this spec proposes and on which the owner has not ruled; and `fullfsync` and `checkpoint_fullfsync` on as the macOS setting (R126). | Not chosen by the owner: diagnose first at the unchanged budget, with automatic checkpointing off and one event transaction for all runs, which was this row's recommendation and the red team's W2; both changes are now part of the design (R26, R126) and of the revised method. Not chosen: build the fallback now, on the first run alone. Still the owner's to choose, inside the direction: other numbers. The report gives, for every population, how many claims exceeded 50, 100, 200 and 1,000 ms, so a stricter budget than the proposed one can be judged from the same data. |
| AP14 | What an imported value may still select (security review S2). Below the data directory, in a backup and in an export the answer is nothing (R101 to R108). It has five points. First, a task's workspace is still `workspaces/<task id>` in the project directory. Second, the import no longer treats in-flight state as 1.0's recovery does. Third, what a restore may do about the processes a backup records (R100): as written, refuse and report, and never signal. Fourth, with that layout two task ids that differ only in the case of ASCII letters are one workspace and one branch on a filesystem that folds case (the re-review's finding S2-R1, F14), so something must refuse the pair or the layout must go. Fifth, whether a backup that a restore is given is taken to be this project's own state when a directory already stands at one of its tasks' exact names (the security re-review's condition C2). | **Decided by the owner, 2026-11-21: as recommended, which settles the first four points; the fifth is still the owner's choice, as said below.** Keep 1.0's workspace layout, with the id checked as one name, links refused and stored paths never used (R106): workspaces that exist at the import keep working, and nothing an agent CLI saw moves. Refuse the import while a recorded local process exists or any remote run is recorded, naming each, and never signal one: the check asks only whether the pid's process or its process group exists, and `--assume-stopped <run id>` is the owner's word for a run that has ended (R119, the red team's blocking finding I1). What the import then closes, it closes in the database only (R105). A restore refuses while a process that the backup or the replaced state records is alive, and signals only those the replaced state records, which this daemon wrote here; a backup's rows never cause a signal (R100, the re-review's condition S2-C2). For the fourth point: refuse the pair by one rule on every platform, and show by a listing, before every use, that a directory or a branch is the task's (R117, R118). No id is lower-cased or rewritten. The cost is one refusal: a 1.0 project kept on a filesystem that tells case apart, and holding such a pair, cannot be imported until the owner removes one of the two tasks with the 1.0 binary, or it stays on 1.0. The dry run names both. 1.0's generated ids make a pair about 7 in 100,000 for 3,000 tasks, and a 1.0 project on a folding filesystem cannot hold one. An export is also refused where a directory already stands under one of its tasks' names; the owner moves it aside, since an export brings no workspace and nothing shows the directory to be that task's. **For the fifth point this spec makes no ruling and recommends neither option, so the answer of 2026-11-21 chooses none, and the owner's choice is open.** R117 is drafted for the first of two options and its clause for a backup is provisional; restore's handling of an existing workspace is not built before the owner has ruled. Option one, a backup is this project's own: a directory at a restored task's exact name is that task's workspace. The restore's documentation and its confirmation then say that the backup is trusted as this project's state, the confirmation lists each directory the restored tasks would own, and the residual risk is stated: a backup made elsewhere that names an existing directory gets it entered by the next run of that task and removed by its cleanup. Its test: `db_backup_restore::restore_lists_the_workspaces_it_adopts_and_needs_confirmation`. Option two, a backup is treated as an export is: a restore refuses while a directory stands at any restored task's name, in any spelling, and leaves it as it was. It fails closed and needs no assumption about who wrote the backup; its cost is that a restore on the machine where the project runs is refused for every task whose workspace still stands, which is the usual state of work in flight or in review, until the owner moves each aside, and that task then starts in an empty workspace. Its test: `db_backup_restore::a_directory_at_a_restored_tasks_exact_name_is_refused_and_left_unchanged` (a self-consistent backup made elsewhere, a canary in the directory). The security reviewer names option two as the one that fails closed. The architect's view, which is not a ruling, is option one with the listing, because a backup made elsewhere already decides every task, prompt and policy of the project it is restored into. | Name workspaces by a hash or a generated name: then no id is a path component anywhere, at the cost of a mapping for the workspaces and branches that exist at the import and of new paths for every agent. For in-flight state: close it without looking at any process, as the first correction had it; no source value then chooses a process to examine, and a run still at work on this machine or on a peer is sent out a second time. The refusal's cost is that a reused pid or a peer that never answers again blocks the import until the owner names the run. For a restore: let `--stop-runs` also stop what only the backup records, for a backup the owner trusts; a pid and a start time do not show who wrote a backup, so a crafted one could have a chosen process stopped. For the fourth point: name workspaces by a hash, with a mapping for those that exist at the import; no import is then refused for a pair, at the cost of new paths for every agent, of branches that either keep the id and its alias or change what a person sees in git, and of this same ownership rule for every mapped old name. Or apply the rule only where the destination folds case: a pair then imports on Linux, and the same database is refused by a restore or an export on macOS, so whether a database is valid depends on the disk under it. Or let an export adopt a directory at a task's exact name, as a 1.0 directory does: a re-import on the same machine keeps its workspaces, and a crafted export can name a directory it wants entered or removed. |
| AP15 | What a source may make the importer do (security review S1). The budgets of "Import bounds" are 1.0's own reader's limits where it has them. Four points go beyond 1.0 or beyond the first draft of this spec. First, three rules are stricter than 1.0's reader for a YAML document that a person edited: an explicit tag is refused (1.0 ignored it), the depth as expanded is 64 (1.0's library allowed 128), and the bytes as expanded are 4 MiB (1.0 had no rule). Corrected on 2026-11-22: under this first point one more rule is stricter than the three it names, and this row did not name it. It is a fourth stricter rule and not the fourth of the four points. A `config.yml` whose stream holds no document is refused, and 1.0 read it as its defaults (F18). The owner's answer was given on the text that said three, and it is not read as covering that rule, which is T21-C1's. Second, a YAML document is bounded at 1 MiB, which is 1.0's own limit, where the first draft said 16 MiB. Third, the import checks the whole source before it writes, and so reads it twice. Fourth, an import from an export is all or nothing with no resume, and an event line that cannot be stored becomes a gap where the first draft stopped the import. | **Decided by the owner, 2026-11-21: as recommended.** Approve the numbers and the four points as a set (R109 to R116). Nothing 1.0 writes meets a stricter rule; a refusal names the file and the rule and says that 1.0 read it, and the dry run lists every such file before anything is written. The second read buys that a hostile or damaged source leaves nothing behind. An export import that was interrupted is run again from the start; raw output lost to a gap is the lowest-value data an import carries, and each gap is recorded and counted. | Match 1.0's reader exactly (tags ignored, depth 128, no byte budget): no document that 1.0 read is refused, a tag is dropped silently as 1.0 dropped it, and one file can still expand to about 65 MiB in memory. Or check and load in one pass: one read of the source, and a refusal part-way leaves an unfinished import behind. Or stop the import at an event line that cannot be stored: no silent-looking loss, and one bad line of raw output blocks a migration. |
| AP16 | A 1.0 daemon started on a project after its import (red team I2). Nothing can stop one while 1.1 is not running, and it would run finished tasks again from the state of import day. | **Decided by the owner, 2026-11-21: as recommended.** Detect it and refuse the next 1.1 start until the owner accepts (R121); say in the upgrade notes what going back loses and how the 1.0 binary is kept (R136). Nothing under `.orchestry/` is written for it. This recommendation rests on F9: while 1.1 runs, the lock keeps 1.0 out. If R31's test shows F9 false on Linux, detection is the only guard while both binaries can run, and the recommendation becomes the 1.0.1 of the next column (red-team condition C4). | Warn and start with dispatch off: no option to learn, and one `/watch` away from running on a forked history. Or a 1.0.1 that refuses to start where `.orch-project` exists: it prevents the fork where this only detects it, and it is a release outside this goal that only the owner can order. |
| AP17 | A source file that the import cannot read or that is over a bound (red team I4) | **Decided by the owner, 2026-11-21: as recommended**, as D3 was on 2026-11-20: a hard stop, a dry-run report by problem class, and no skipping and no setting aside. Which classes go on instead of stopping is the table "Import problem classes"; the merge confirms its rows and the wording of R136. The rows added on 2026-11-22 (`index_entry`, `index_duplicate`, an input or output error, and the two rows of `config.yml`) were not in the table when the owner answered; the two index rows are T21-X1 and T21-X2, and neither is answered. Stop, with every such file named by the dry run before anything is written (R123). The owner repairs the file after taking a copy, and keeps the 1.0 binary until the import has finished (R136); that the command which installs 1.0 beside 1.1 exists and works is a recorded check, and without it the red team's I4 is open again (its condition C3). | An option that skips a named file or sets it aside, and records that: the import always finishes, a task or a run is then absent from 1.1 unless the report is read, and setting files aside is the quarantine that 1.2 owns. |
| AP18 | What the writer does when the database fails under it, and whether a finished run survives that (red team W3) | **Decided by the owner, 2026-11-21: as recommended.** The classes of R128 and the finish spool of R129. Their times (50 ms to 2 s, 60 s) are proposals with no measurement behind them. The closure corrections added two rules that belong to this point: the 60 s is counted on one transaction being retried, so an idle writer never fails; and a finish whose spool write fails is kept in memory, retried and reported, and is lost only when the daemon is stopped before the file or the commit succeeds (W3-R1). | Stop the daemon at the first failure: simplest, and every run in flight is cancelled and its result lost, as in 1.0. Or keep finishes in memory only, as the first draft did: no new file, and a restart behind a locked database loses results that the draft called never dropped. |
| AP19 | What `orch db query` may read, and where a backup may be written (security review S3, S4) | **Decided by the owner, 2026-11-21: as recommended.** Fixed diagnostic views under an authorizer that denies by default (R74, R130), and backups only inside the data directory (R75). The view names and the function list belong to O9. | Every table, read-only, as the first draft: better for diagnosis, and it shows token hashes and any later credential table to whoever may run the command. Or a backup destination outside the data directory under the same private-parent rules: one command for a copy on another disk, and every rule must then hold on a directory orch does not own. |
| AP20 | The shape of the release (red team, "the path not taken"). 1.1 delivers the engine's move to port v2, protocol 3 and the one-way import on the same day. | **Decided by the owner, 2026-11-21: as recommended**, as D7 was on 2026-11-20: one phase 1.1, the ordered PR stack, no release in between. Neither alternative of the next column is chosen, and binding decision B1 excludes the second unless the owner amends B1. Keep one release 1.1 and the ordered PR stack: `main` stays on files until the switch, and each PR before it can be reverted alone. This is the plan as written; it is a point here because the first draft did not put the alternatives to the owner. | Cut a release after plan task T20, with the engine on port v2 over files, protocol 3 and authorization, and make 1.1 the switch, the import and `orch db` alone: a regression in the engine can then be told from an import defect, at the cost of one more release and its compatibility window. Or ship 1.1 with SQLite chosen for each project and files as the default: field use of the importer before any project must convert, two tiers of guarantees for one release, and a change to binding decision B1. |

### Points raised on 2026-11-22 and not answered

The compatibility evidence on the import's bounded reader
([issue 146](https://github.com/yuxhuang/r-orch/issues/146)) raised five points. They were put to the owner
on 2026-11-22 and none is answered (record J of "Owner's answers"). The first four each carried a default for
the case that the owner does not answer, and each default keeps a stop that this spec or the bounded reader
already had; this text is drafted from those four defaults. The fifth carried none, and this text is
drafted from the proposal in its row. The column "Recommended" is the spec owner's recommendation and is nobody's answer: on T21-C1, T21-C2
and T21-X1 it is not the default that this text drafts. An explicit answer of the owner replaces a default,
through a PR that changes the rows named in the last column.

| # | Point | Recommended by the spec owner | The other option | Drafted in this text | Changes with the answer |
|---|---|---|---|---|---|
| T21-C1 | A `config.yml` that is present and whose stream holds no document | **A.** Import it exactly as `{}`. 1.0 reads it as its defaults and tests that (F18); the file holds no content, so nothing is discarded; and the refusal asks the owner to edit the directory that is the way back (R136) to state what 1.0 already assumed. Every bound and the single pass stay as they are | **B.** Keep the refusal under `documents`, as a fourth place stricter than 1.0, and say that 1.0 read the file | **B, the default.** Not an answer | R70, R109, R110, "Import rules" (the table of `config.yml`, its row in "Import problem classes" and the repair under that table), the row of the document count in "Compatibility with 1.0", C-V2 to C-V4, C-V13, AP15, ADR 0017 decision 21 |
| T21-C2 | A `config.yml` whose one document is null | **A.** Import it as `{}`, by the same sentence: an empty stream and a null document hold no content, and a scalar or a sequence holds some. Confidence is lower than on T21-C1, and the two can be answered apart | **B.** Keep the type stop and say that 1.0 read the file | **B, the default.** Not an answer | R70, "Import rules" (the same three places), C-V6 |
| T21-X1 | An index array with an entry that is not an object or has no string identity | **A.** `index_unreadable`, and the import goes on in id order, as for an index that does not parse. 1.0 treats the two alike (F19), no entity is lost, and otherwise the more damaged file gets the gentler rule. Answered A together with T21-X2 answered A, it leaves the pair that T21-X2's row describes | **B.** Stop, as `index_entry` | **B, the default.** Not an answer | R65, R122, R123's list of tests, "Order", the row `index_entry` and the repair under that table, the `rule` list of the `--json` report, X-V4, X-V6 |
| T21-X2 | An index array whose every entry has a string identity, with one identity twice | **A.** Stop, as `index_duplicate`, naming the identity. Where every entry of the array is a complete entity, 1.0 still uses this index (F19), so its order is live and it is ambiguous; the first and the last occurrence are both guesses that a one-way import would make permanent. Where an entry is not complete, 1.0 rebuilds the index and its order is already dead; the import stops there too, because it judges identities alone and cannot tell the two cases apart, and decoding entries to tell them apart would make the index's content an input again (R122). No 1.0 writer produces either state. For the owner, added on 2026-11-22 (the first draft said only that 1.0 still uses this index): the line between T21-X1 and T21-X2 is not the line 1.0 draws. If both are answered A, `[{}]`, which 1.0 rebuilt, goes on in id order, while `[{"id":"b"},{"id":"b"}]`, which 1.0 also rebuilt, stops; the reason given for T21-X1, that the more damaged file should not get the gentler rule, then speaks against this pair. Under the two defaults drafted here both stop, and the rows do not disagree | **B.** `index_unreadable`, and the import goes on in id order. That drops an order that 1.0 used where every entry was a complete entity; where one was not, 1.0 drops that order itself when it next reads the index. Not recommended | **A, the default.** Not an answer | R65, R122, R123's list of tests, "Order", the row `index_duplicate` and the repair under that table, the `rule` list of the `--json` report, X-V5, X-V7 |
| T21-O1 | What a known key that `config.yml` does not set imports as. It decides `run_cap` for a missing file, for `{}` and for a mapping without `scheduling.max_concurrent_agents` | **A.** The value 1.0 ran the project with, which is 1.0's default: `run_cap` 6. "Import rules" has the table of cases and the reasons | **B.** Nothing: only the keys the file sets are imported, the 1.1 default of each other setting applies, and there is no `run_cap` | **A, a proposal.** No default came with the question. Not an answer: the owner ratifies A or chooses B | R65 (the note on a default), R70, "Import rules", C-V7, C-V14 |

Not questions, and confirmed with their rows when the owner merges the PR that carries them: that a missing
`config.yml` is the empty mapping; that the refusal of a `config.yml` which is a scalar or a sequence says
what 1.0 did; the corrections of fact in R110, in the row of the document count, in AP15 and in ADR 0017
decision 21; the whole-array rule and the order of the table under "Order"; that bounds, input and output
errors and the file kind are terminal; what each refusal says 1.0 did, the two cases of `index_duplicate`
included; and the names `index_entry` and `index_duplicate` (O9).

### Decisions asked beside the spec's points

The approval request of 2026-11-20 asked eight decisions about the reviews and the verification, D1 to D8,
beside O1 to O10 and AP1 to AP13. They are restated here in this spec's words, with the points each one
overlaps. A decision and a point that overlap are still two questions.

| # | Decision asked | Recommendation of the request | Overlaps |
|---|---|---|---|
| D1 | The blocking review findings | Require the bounded parsing and export contract (S1), names below the data directory that only the daemon makes (S2), the import's refusal while local or remote work may survive (I1), and a re-review at the exact head; no override and no product start | AP14, AP15; R101 to R120 |
| D2 | Safety of the cutover | Detect a 1.0 state that changed after the import without writing to it; take an entity's file over a stale index and report the difference; recover an interrupted pointer write; no automatic repair that destroys | AP16; R121, R122, R125 |
| D3 | A source file that cannot be read or is over a bound | Keep the hard stop and a dry-run report by problem class; inspect and repair only in a separate copy; keep the 1.0 binary installed. No skipping and no setting aside | AP17; R123, R136 |
| D4 | What the writer guarantees | State the policy for a busy, failing, full or corrupt database, bounded retries and shutdown, and a finish that survives a restart; state what a failure of that persistence does | AP18; R128, R129 |
| D5 | The security review's major conditions | Fixed diagnostic views under an authorizer that denies by default, private backups inside the data directory, attention items without outside text, named paths that turn trust on, isolation tests for request keys | AP19; R74, R75, R130 to R134 |
| D6 | Access for verification | A real suspend on an isolated macOS machine, evidence from native Linux, the peer check on a second host, and a copy of the live project outside the repository for R71 later | "Verification environment"; R42, R5 (deferred on 2026-11-21: record I), R63, R89, R71 |
| D7 | The sequence of the release | One phase 1.1 and the ordered PR stack; no release in between | AP20 |
| D8 | Delivery of the entry steps | Finish the validation and the PRs of the entry steps and the reviewers' conditions before the spec is approved; the spec's PR stays a draft meanwhile | plan tasks T1 to T7 |

### Owner's answers

One row for each id. The record has eleven parts, named by a letter in the table; the tenth records that no
answer was given, and the eleventh a ruling on platforms that answers no id:

- **A.** The owner's first answers of 2026-11-20, to six points. Their message could not be read again
  when this table was written. Nothing here quotes it; the six rows are carried from the earlier revision
  of this table and rest on D.
- **B.** The owner's message to the lead of 2026-11-20, 19:57 UTC (`msg_laRzEil`), kept word for word:
  O3, AP12, D3, D6 and D7.
- **C.** The owner's message of 2026-11-20, 20:00 UTC (`msg_C6_S8W8`), kept word for word: the direction
  on AP13.
- **D.** The owner's instruction to the lead of 2026-11-21, 02:05 UTC, kept word for word in the lead's
  record. The message (`msg_c0_Pc8X`) says that all 31 ids of the approval request and AP14 to AP20 are
  answered; the checkpoint task written from it says how: as recommended, except AP13, whose workload and
  budget are revised with the numbers to be proposed, measured and then approved by the owner. The
  instruction names one more message, which could not be read either and is not quoted here. D alone is
  the record for every id that has no message of its own.
- **E.** The owner's ruling of 2026-11-21, about 14:05 UTC, kept word for word in the lead's record. The
  owner was shown two ways to read a miss of the revised budget and answered "1.": every miss is kept and
  reported; a miss selects the file fallback only when it is attributable to events; a miss without
  events, or on one platform alone, selects no event path and goes back to the owner. The record says
  that the owner knows this amends a sentence of C. Minutes later (recorded 14:07 UTC) the owner answered "yes" when
  asked whether the review corrections of this revision may be written in the same pass. That word allows
  the writing. It answers no id, and it accepts no finding, method, number or result.
- **F.** The owner's two answers of 2026-11-21, recorded 16:49 UTC, kept word for word in the lead's
  record. The owner was shown two questions in plain words. The first was how "one platform alone" is
  judged, by cell or by kind of limit and population, with the case in which the two differ: on macOS a
  C24 claim at 230 ms in a checkpoint window and no event over its age, on Linux a C24 event older than
  5,000 ms and no claim over a limit. The second was whether a data set that holds a defect and a miss
  may be followed by a data set that fixes the defect without the owner's word. The owner answered "kind
  reading is fine. ask first." By the first sentence the kind reading is chosen: two different kinds of
  miss in one cell, one on each platform, select no path and go back to the owner, and the cell reading
  is not adopted. By the second, any miss of a mandatory repetition that is `valid` or `host_not_idle`
  requires the owner's word before a further data set, whatever the headline, a data set that only fixes
  a defect included; there is no exception. The record allows both answers to be written into this
  spec, its plan and ADR 0017. They accept no number, result, proxy for cause, eligibility rule, method
  or publication, and they permit no measurement. They close no review finding. When they were written
  into this spec no independent review had read the text that carries them; the five reviews of
  `d2fe9b0` read it afterwards. Whether the other conditions that the reviews of `3c50293` left
  may be corrected in the same pass was asked apart and is no part of F; the revision that carries F
  corrects none of them.
- **G.** The owner's word of 2026-11-21, recorded 16:51 UTC, kept word for word in the lead's record. The
  owner was asked whether the architect may also correct what the reviews of `3c50293` had left (the
  restore's recovery, the durability of a stop, one wrong attribution) in the same pass, and whether
  corrections for review findings on this spec may always proceed without a new question, as long as
  the result still comes to the owner as a pull request. The owner answered "yes always". Those two
  words are all that the owner said. The lead's record reads them as a standing rule for this spec, its
  plan and ADR 0017, and states its limits: a finding that needs a choice between policies, a number, a
  trust assumption or a change to something the owner decided still goes to the owner as a question; the
  rule accepts no finding, review, method, number or result and waives no check; every changed text
  needs its own review; and the text reaches `main` only through a pull request that the owner merges.
  G answers no id.
- **H.** Five of the six answers of 2026-11-21 in the lead's record, which keeps them word for word: its
  items 1 to 4 and 6. Its item 5 is about a test fixture and is no part of this text. They were given under
  the owner's delegation and are not the owner's own words. The
  owner's words are the delegation, to the session that supervises this work for the owner: "please
  supervise this development; you have permission from me if your recommendation is the same as the lead;
  only escalate to me if you have diverge opinion; and continue to monitor the same cadence." The lead wrote
  one recommendation on each of four things that this spec had put to the owner, two as proposals and two as
  questions that it had left open;
  the lead's record names them AP13-KINDS, AP13-WHOLESET, AP13-BOUNDARY-1 and AP13-BOUNDARY-2. The session
  read the recommendations in full. It withdrew a rule of its own, by which any mandatory miss in a repetition
  that is `valid` or `host_not_idle` would have stopped the
  measurement and selected nothing, with the kinds as names in a report only, because that rule would have
  amended the owner's ruling of E, which the delegation does not allow. It then recorded that its
  recommendation is the same as the lead's on each point, so that by the delegation these are answers.
  First, the five kinds are adopted for applying the kind reading, and they are operative: the regular
  claims, the claims of a checkpoint window, the claims of a burst window, an event's age, and a refusal or
  something still queued, with the cell and the population kept in the match. Second, in the record's words:
  "adopt the
  whole-data-set veto: any miss covered by selector steps 2-4 anywhere prevents the data set from selecting
  an event path. An eligible set with only qualifying same-kind event-population misses still reaches the
  existing fallback rule." This spec reads "same-kind" miss by miss, as step 3 does: every miss is shown by
  both platforms in the same cell, population and kind, and a data set may hold one such miss or several, of
  one kind or of more than one (R63, "What a miss selects"). That reading is this spec's and is not in the
  record's words; the method review of `ec68890` traced it as what step 5 gives. Third, starting the second
  platform's block
  after a mandatory miss on the first platform in a repetition that is `valid` or `host_not_idle` is further
  measurement and needs the owner's word. Fourth, replacing an `invalid` or `host_not_idle` set of five
  while the data set already holds such a miss is further measurement and needs the owner's word; the old
  sets and every miss are kept. Fifth, one new pull request for this corrected text may be opened as a draft
  from the reviewed correction once its publication checks and its disclosure package, which is its
  statement of what it does not decide, are ready (the row D8). The record states the
  limits: the answers approve no threshold, number, result, eligibility rule, proxy for cause, method or
  campaign; they waive no check and close no review finding; "ask first" stands; the reviews stay
  independent; every changed text needs its checks and its review; the holds on product work and on
  entry-step deliveries are unchanged; and no merge is authorised. That the answers permit no measurement is
  this spec's reading of those limits and of "ask first", and the record does not say it in those words. The
  same record says what it does not answer: the
  other proposals of AP13 (the numbers, the results, how the lanes and the collector sit beside the
  proposal, the sync settings, the eligibility rule, the reading of "a burst alone", the host and platform
  rules), the fifth point of AP14, and the word on the live copy for the plan's task T4c.
- **I.** The owner's decision of 2026-11-21 on latency, kept in the lead's record. The lead had asked which
  data set, workload, limits and platforms the owner authorises next for deliverable D, after the first run's
  miss. The owner's words, as the record gives them: "dont' worry about latency. we don't care about it now.
  don't over engineer and over optimize at this phase". The record then states what follows from them, in the
  words of the session that supervises this work for the owner, and this spec restates it. Deliverable D is
  dropped as an entry step and as a gate of release 1.1, and no data set is authorised. Events go into SQLite
  by the batched design that this spec already states, with no claim-latency budget to meet in this phase and
  no selection procedure to apply, and the file fallback for events is not built in 1.1. The owner's earlier
  ruling and the answers on AP13 are not withdrawn: they have nothing to act on until latency is taken up
  again. The first run's figures are kept in one tracked issue
  ([issue 98](https://github.com/yuxhuang/r-orch/issues/98)). And the measurement, the budget and the
  selection procedure are marked as deferred in this spec, its plan and ADR 0017, not deleted, with
  deliverable D taken out of the release gates. The same record keeps the tests of correctness and of data
  safety and the other gate lines as they were, and asks that the product be built as the simple version this
  spec describes: no speculative performance work, no tuning, no abstraction or setting that this spec does
  not ask for, and no benchmark as a gate. I defers what AP13 still held. It accepts no number, method,
  eligibility rule or result, it closes no review finding (those that exist only for the measurement are
  parked), and it answers no other id.
- **J.** No answer. The lead's decision record of 2026-11-22, made at 19:24 UTC, says that five points of
  this spec were put to the owner (T21-C1, T21-C2, T21-X1, T21-X2 and T21-O1) and that the owner had not
  answered them when it was made. For the first four it gives the default that each question carried for
  that case: B, B, B and A. For the fifth it gives none, and asks that the effect on `run_cap` be shown
  to the owner with the text, for a missing file, an empty mapping and a partial one. The same record says
  that the owner ratifies the drafted rows by merging the PR that carries them, and that an explicit answer
  given later replaces the default it speaks to. J is the absence of an answer. It is no word of the
  owner, it approves no row, and a default is not a choice that the owner made.
- **K.** The owner's ruling of 2026-11-23 on platforms, given directly to the session that supervises this
  work for the owner. Its first form covered release 1.1: develop, test and qualify it on Linux only, and
  defer every macOS-only failure, check and compatibility gap. The owner then widened it, in the words as the
  session recorded them: "i want them out of the whole development of 2.0. we can work with macos
  compatiblity after 2.0". This spec reads the two together as follows, and restates no more than the
  record says.
  - *Scope.* Every phase from 1.1 through 2.0, inclusive (1.1, 1.2, 1.3, 1.4, 1.5 and 2.0), is developed,
    tested, gated and qualified on Linux only: GitHub's Linux CI, the Linux development hosts and the Linux
    release gate.
  - *Not required before 2.0.* A physical macOS suspend and wake (a physical macOS observation is issue 178); Darwin socket and process-ownership
    behaviour; APFS and a case-folding macOS volume; the macOS leg of the peer matrix; a macOS cold gate; a
    macOS hardware measurement. None is an acceptance row, none makes a `must` row `env-limited`, and none
    needs an owner waiver. Where a row, a cell or a note of this spec names a macOS run as a check still to be
    made, that part is withdrawn and the Linux part stands. Where it states what a macOS run measured, or what
    the product does on macOS, it is history and stays.
  - *Unchanged.* The peer check on a second Linux host (R89); the clock semantics and the clock-seam tests
    (R42, and the prototype's wake and wall-jump tests), which prove renewal ordering and nothing about a real
    suspend; the real suspend and resume of an isolated, authorized Linux host with a supervised run in flight
    (R42, issue 110; pending, not run, and no host is suspended without the owner's permission for it); the eight gate lines G1 to G6, G8 and G9, each proved
    on Linux; the owner's permission and the limits on private content for the live copy (R71, plan task
    T4c); the regression oracles, which are the conformance suite on both backends and the file reference
    (R2, R3, R56) and the case-fold rule of R117; Linux CI as the gate, the review rules and owner-only
    merges.
  - *Where macOS findings go.* A macOS-only failure or gap is a GitHub issue in the
    [After 2.0](https://github.com/yuxhuang/r-orch/milestone/20) milestone, unless triage assigns another milestone, and the macOS checks that stood open for
    1.1 stay open there. A defect that Linux shows as well goes through the normal Linux review and CI.
  - *History and the retired experiment.* The first event measurement of 2026-11-20 (deliverable D, with its
    miss on macOS), gate line 7 and the AP13 method were retired or deferred by record I, and K does not
    change that: K only puts any revival on Linux. Evidence taken on macOS before K is kept as recorded and
    as what it measured: the lease prototype's 21 checks on macOS arm64, the first event run in issue 98, the
    recovery fixtures on two APFS volumes, and the library probes. It is not a current acceptance row.
  - *What K does not do.* It removes no code, changes no CI and no status line, and decides nothing about macOS
    support after 2.0.

"As recommended" means, for O1 to O10, AP1 to AP13 and D1 to D8, the recommendation and its condition in
the approval request of 2026-11-20; and for AP14 to AP20, which that request did not hold, the
recommendation column of this spec's own row. An answer is the owner's word as recorded and no more. It is
not an approval of this spec, of a number, of a result, or of text beyond the recommendation, and the
record does not say which revision of a row the owner read. The owner confirms this table, and the text of
every row, by merging the PR that carries it. The merge of 2026-11-21 carried the first-review text, which
has neither this table nor the corrected rows, and confirms none of them.

Counted over the 38 ids: 37 are answered as recommended, and AP13 is answered in its direction only. Two
things inside those answers are still the owner's: every number and result of AP13, and the fifth point of
AP14. A third thing lies outside the answers and is the owner's as well: the word on using the live copy
for plan task T4c, which neither D6 nor the answer to AP8 gives. No id of the 38 is pending. Five ids were
added on 2026-11-22 and none of them is answered (record J): their rows stand at the end of the table and
say what this text drafts meanwhile. The last column says
what an answer does not settle: a proof, a review or a
delivery that is still due, or a part the owner still holds. Since record I the first of the two things,
every number and result of AP13, is deferred with the measurement: it stays the owner's, and nothing in
1.1 waits for it.

| Point | Answered | Record | Answer | Not settled by the answer |
|---|---|---|---|---|
| O1 | 2026-11-20 | A, D | As recommended: `.orch-project` in 1.1, the 1.0 search order kept, `--project` and `ORCH_PROJECT` in 1.5 | The conditions on copies, worktrees and an interrupted pointer write are R34, R135 and R125 |
| O2 | 2026-11-21 | D | As recommended: the 33 tables with the columns that later phases need, and the other 17 deferred. The recommendation's condition: the exact schema of this spec, with its foreign keys and integrity rules, is what is approved, and a count of tables is not | The merge of the PR that carries this corrected text confirms the schema as written |
| O3 | 2026-11-20 | B, D | As recommended: the import runs with both daemons stopped and is refused while local or remote 1.0 work may survive; orch never deletes 1.0 files, and the manual gives the advice on keeping them |  |
| O4 | 2026-11-21 | D | As recommended: the saved setting is kept; dispatch is off after an import, a restore and a migrating start; the flag overrides; the `dispatch_off_with_work` item stays |  |
| O5 | 2026-11-21 | D | As recommended: one timer with a due time for each job that survives a restart, and a bounded shutdown | The writer's failure policy, which the recommendation asked for before the build, is R128 (AP18). The bounded shutdown is R139, written after the answer from the resilience review; its times are proposals |
| O6 | 2026-11-21 | D | As recommended: resolved items are kept 90 days; a blocking item makes `orch doctor` exit 1 and a warning does not; doctor's existing exits do not change |  |
| O7 | 2026-11-21 | D | As recommended: unknown keys are kept as an inert legacy object of at most 64 KiB and listed by the dry run; `max_concurrent_agents` becomes the optional `run_cap`; input over the limit stops the import and the source is not edited | What a known key that `config.yml` does not set imports as, and so `run_cap` for a missing file, for `{}` and for a mapping without the key: T21-O1, not answered |
| O8 | 2026-11-21 | D | As recommended: keys are kept for one hour and the expiry job ships in 1.1 | The recommendation's tests of scope and its exclusion of credential operations are R134 |
| O9 | 2026-11-21 | D | As recommended: the names of this spec approved as a set, with the names that accepted review corrections added | A name added after this revision is not covered: `index_entry` and `index_duplicate`, added on 2026-11-22, are proposals. A rename after the protocol PR costs a protocol bump |
| O10 | 2026-11-21 | D | As recommended: a hand-written invented corpus for conformance, a seeded generated corpus and damaged-input cases for the importer, and only aggregate sizes from a private copy | The check on the live copy is still due (R71) |
| AP1 | 2026-11-20 | A, D | The two `orch-peer` source changes are allowed; wire format and files unchanged | The proof on a second host is still due (R89); since record K that host is a Linux host |
| AP2 | 2026-11-20 | A, D | The 111-operation catalogue is the baseline, plus the 12 additions |  |
| AP3 | 2026-11-21 | D | As recommended: an agent's session stays an owner session for the 1.0 operations, and the new owner-only operations are refused in agent or MCP context. This attributes a call; on one uid it does not authenticate it |  |
| AP4 | 2026-11-21 | D | As recommended: the CLI and `--json` return every row in the 1.0 shape by default; `--limit` and `--cursor` ask for one page |  |
| AP5 | 2026-11-20 | A, D | On for a new project, with the trust statement, turned on only in the owner's context |  |
| AP6 | 2026-11-21 | D | As recommended: 1.1 writes `workspaces/`, orch-peer's files and, at `orch init`, `workspace-exclude` and `.gitignore`; never a 1.0 state file |  |
| AP7 | 2026-11-20 | A, D | Import, restore and vacuum may run offline under the locks; the other `orch db` commands use the local socket |  |
| AP8 | 2026-11-21 | D | As recommended: an atomic operation over the bound runs whole and is counted, and no lane wait is promised while one runs; the figure the first draft cited is withdrawn | The count of the largest cascade is still due (plan task T4c), and its use of the live copy still needs the owner's word: neither this answer nor D6 names the copy for it |
| AP9 | 2026-11-21 | D | As recommended: no scheduled backup in 1.1, and a checked copy before every pending migration |  |
| AP10 | 2026-11-21 | D | As recommended: the 100-entry cap and its silent drop go; the retry table has no cap |  |
| AP11 | 2026-11-20 | A, D | A random restore lineage and a counter; gate line 9 amended to "no lineage and counter pair is reused" | The lease prototype's report is still to confirm the mechanics (R6) |
| AP12 | 2026-11-20 | B, D | As recommended: a suspend ends no co-located, supervised run in 1.1, and a holder's own deadline arrives with workers in 1.5 | The proof on a real suspend was due (R42); record K withdraws only its macOS form, and the Linux check stays pending (issue 110) |
| AP13 | 2026-11-20, the direction only; 2026-11-21, what a miss selects; 2026-11-21, how "one platform alone" is judged and what may follow a miss; 2026-11-21, under the owner's delegation, the list of kinds, the whole data set and what counts as further measurement; 2026-11-21, the measurement, the budget and the selection deferred | C, D, E, F, H, I | **Deferred on 2026-11-21 (I):** claim latency is not a concern of 1.1; no data set is authorised; events are stored in SQLite by the batched design (R63); the fallback is not built; gate line 7 is retired. The answers before it, kept and not withdrawn, with nothing to act on until latency is taken up again: Revise the workload and the budget and measure anew, not the diagnosis first. Changing the budget is approved in principle; no number is. If the revised budget is missed the fallback applies, as amended on 2026-11-21 (E): only for a miss attributable to events; a miss without events or on one platform alone selects no event path and goes back to the owner; every miss is kept. As answered on 2026-11-21 (F): "one platform alone" is judged by kind of limit and population, not by cell, so two different kinds of miss in one cell, one on each platform, select no path and go back to the owner; and after a mandatory miss in a `valid` or `host_not_idle` repetition no further data set is started without the owner's word, whatever the headline, a data set that only fixes a defect included. As answered on 2026-11-21 under the owner's delegation (H): the five kinds are operative in that judgment, with the cell and the population kept in the match; a miss of steps 2 to 4 anywhere in a data set keeps it from selecting an event path, and an eligible data set in which every miss is a valid miss in a population that holds events, shown by both platforms in the same cell, population and kind, still selects the fallback; starting the second platform's block after a mandatory miss on the first, and replacing an `invalid` or `host_not_idle` set of five while such a miss stands, each need the owner's word | Since I, deferred with the measurement and not due in 1.1: everything this cell lists. I accepts no number, method, eligibility rule or result and settles none of the open points. As the cell stood: **Still the owner's:** every number of "Event measurement"; how lane 1's 50 ms, lane 3's 1 s and the collector's 256 events or 250 ms sit beside it; the macOS sync settings; and the results on macOS and on native Linux hardware, none of which exists yet. The first run's FAIL against 50 ms and 100 ms is kept. Not ruled on by the owner: the rules that this spec writes around the ruling and that no answer has settled ("The verdict" with its axes, headlines and exits; "What a miss selects" with its eligibility rule, its reading of "a burst alone" and its step from "The host"; "The host"; the platform classes) and their proposed limits. The two answers of F and the four of H choose rules and accept none of those. The four of H were given under the owner's delegation and are not the owner's own words. Settled by no answer: what is done with a block that is already running when a mandatory miss is seen and whether the blocks of two platforms may overlap or be run in turns, before which no measurement campaign is started; and which data set a block that the owner allows belongs to. A path that is selected is not by that permitted to be measured or qualified to ship |
| AP14 | 2026-11-21 | D | As recommended, which settles four points: the 1.0 workspace layout is kept, with the id checked and links refused; the import is refused while a recorded local process or any remote run exists, and nothing is signalled; a restore refuses and reports, and never signals because of a backup's rows; and two ids that differ only in case are refused by one rule on every platform | **The fifth point is still the owner's choice.** The row recommended neither option, so "as recommended" chooses none: a backup trusted as this project's own for a workspace that already stands, with the directories listed in the confirmation; or a restore refused there, as an import from an export is, which the security reviewer names as the option that fails closed. R117's clause for a backup stays provisional, and that part of restore is not built before the ruling |
| AP15 | 2026-11-21 | D | As recommended: the numbers of "Import bounds" and the four points, as a set | A fourth stricter rule, which the text the owner answered did not name: a `config.yml` whose stream holds no document is refused, and 1.0 read it as its defaults. T21-C1, not answered |
| AP16 | 2026-11-21 | D | As recommended: a 1.0 state that changed after the import is detected and the next 1.1 start refused until the owner accepts it; the upgrade notes say what going back loses | The recommendation rests on F9. If R31's test shows F9 false, it becomes the 1.0.1 alternative and goes back to the owner |
| AP17 | 2026-11-21 | D | As recommended, and as D3: a hard stop, with every such file named by the dry run before anything is written | The command that installs 1.0 beside 1.1 is a recorded check still due (R136); without it the red team's I4 is open again. The rows added to "Import problem classes" on 2026-11-22 were not in the table the answer covered: T21-X1, T21-X2, T21-C1 and T21-C2, not answered |
| AP18 | 2026-11-21 | D | As recommended: the failure classes of R128 and the finish spool of R129, with the idle-writer and spool-failure rules | Their times (50 ms to 2 s, 60 s) have no measurement behind them |
| AP19 | 2026-11-21 | D | As recommended: fixed diagnostic views under an authorizer that denies by default, and backups only inside the data directory |  |
| AP20 | 2026-11-21 | D | As recommended, and as D7: one release 1.1 and the ordered PR stack |  |
| D1 | 2026-11-21 | D | As recommended: the bounded parsing and export contract, names that only the daemon makes, the import's refusal while work may survive, and a re-review at the exact head; no override and no product start | The re-review at the exact head of this revision is still due |
| D2 | 2026-11-21 | D | As recommended: a changed 1.0 state is detected without writing to it; an entity's file is taken over a stale index and the difference reported; an interrupted pointer write is recovered; no automatic repair that destroys |  |
| D3 | 2026-11-20 | B, D | As recommended: a hard stop and a dry-run report by problem class; no skipping and no setting aside |  |
| D4 | 2026-11-21 | D | As recommended: the policy for a busy, failing, full or corrupt database, bounded retries and shutdown, a finish that survives a restart, and what a failure of that persistence does | The order and the bound of a stop (R139) and the policy for the other connections (R140) were written after the answer, from the resilience review; their times and counts are proposals with no measurement behind them |
| D5 | 2026-11-21 | D | As recommended: the security review's major conditions, as R74, R75 and R130 to R134 | The library features they need are audited in the revised report of deliverable C, which is not yet delivered (ADR 0017 decision 19) |
| D6 | 2026-11-20 | B, D | As recommended: a real suspend test on an isolated macOS machine, evidence from native Linux, the peer check on a second host, a copy of the live project outside the repository for R71 later; no private content in the repository or a PR | Every one of those checks was due, and no native Linux host is named. Record K withdraws the macOS suspend test and moves the real-suspend check to an isolated, authorized Linux host (issue 110, pending); the other three stand |
| D7 | 2026-11-20 | B, D | As recommended: one phase 1.1, the ordered PR stack, no release in between |  |
| D8 | 2026-11-21; 2026-11-21, under the owner's delegation, that the follow-up PR may open as a draft | D, H | As recommended: the validation and the PRs of the entry steps and the reviewers' conditions are finished before the spec is approved, and the spec's PR stays a draft meanwhile | The entry steps B, C, D and E are not yet delivered through their PRs. The answer was given while the spec's PR was an open draft. The owner merged that PR on 2026-11-21 at its first-review text, and the record does not say that the merge approves the spec or changes this answer; it is read here as standing for the follow-up PR that carries this corrected text. By an answer of 2026-11-21 given under the owner's delegation (H), one new PR for the corrected text may be opened as a draft from the reviewed correction once its publication checks and its disclosure package (its statement of what it does not decide) are ready. A draft is not accepted content: every condition of this answer still stands before the spec is approved and before that PR merges, merging it chooses no proposal that is still pending, accepts no number or result and releases no product work, and the merge is the owner's alone. When it merges is the owner's to rule. Since 2026-11-21 deliverable D is dropped (I) and is not among the entry steps that this answer waits for |
| T21-C1 | Not answered | J | None. Drafted from the default B: a `config.yml` whose stream holds no document is refused, and the refusal says that 1.0 read the file as its defaults | The answer. The spec owner recommends A: import it as `{}` |
| T21-C2 | Not answered | J | None. Drafted from the default B: a `config.yml` whose one document is null stops on its type, and the refusal says the same | The answer. The spec owner recommends A: import it as `{}` |
| T21-X1 | Not answered | J | None. Drafted from the default B: an index array with an entry that has no string identity stops, as `index_entry` | The answer. The spec owner recommends A: `index_unreadable`, and the import goes on |
| T21-X2 | Not answered | J | None. Drafted from the default A: an index array with one identity twice stops, as `index_duplicate` | The answer. The default and the spec owner's recommendation are the same here. The row of T21-X2 says what 1.0 did with such an array, in its two cases, and what answering T21-X1 and T21-X2 both A leaves; that is for the owner to read before answering either |
| T21-O1 | Not answered | J | None, and no default. Drafted from the proposal A: a known key that `config.yml` does not set takes the value 1.0 ran with, so `run_cap` is 6 | The answer: the owner ratifies A, or chooses B, under which such a project has no `run_cap` |

## Gate lines

The gate of release 1.1 is the eight lines G1 to G6, G8 and G9. The owner retired G7 on 2026-11-21
(record I of "Owner's answers"); what it said is kept under the table as a record. Since 2026-11-23 (record K)
the eight lines are proved on Linux, and through 2.0 on Linux only (G2's copy of the live project, and the owner's permission for it, are unchanged): a macOS result is neither required for a
line nor a reason to withhold one, and a defect that Linux shows as well is handled as any Linux defect.

| Gate line | Proved by |
|---|---|
| G1 Conformance on both backends, with order and rotation | R2, R3, R56: `cargo test -p orch-engine --test storage_conformance` and `cargo test -p orch-engine --test order_conformance`, each run for SQLite and for the file reference; Linux |
| G2 Importer on a copy of the live project | R71: `cargo xtask import-verify --source <copy> --record <dir>`; recorded check on a Linux host, from the authorized copy |
| G3 Backup then restore as one restorable point | R75, R76: `db_backup_restore::backup_then_restore_round_trips_database_blobs_and_repos` (the repositories directory is empty and still compared) |
| G4 Export then import reproduces every row and hash | R80: `db_export_import::export_then_import_reproduces_every_row_and_hash` |
| G5 Worker and task principals are refused | R53: `ops_authorization::worker_is_refused_what_the_matrix_denies`, `::task_is_refused_what_the_matrix_denies` |
| G6 A second daemon refuses and names the holder | R30: `daemon_data_dir::second_daemon_refuses_and_names_the_holder` |
| G8 A repeated change runs once; a changed body is refused | R45, R46: `ops_request_keys::a_repeated_change_runs_once`, `::same_key_with_a_different_body_is_refused` |
| G9 Restore starts a new generation, reuses no pair of lineage and counter, lists and confirms. The owner amended this line on 2026-11-20 (AP11); the goal's text, quoted in Source, said "no epoch is reused" | R77, R78, R132: `restore_fencing::no_lineage_and_epoch_pair_is_reused_after_restore`, `::an_earlier_lineage_is_refused_after_a_restart`, `::restore_lists_reenabled_identities_and_tokens`, `::restore_without_confirmation_changes_nothing` |

G7, retired on 2026-11-21: no gate of release 1.1, and nothing proves or fails it. The number is kept for
references to the first run ([issue 98](https://github.com/yuxhuang/r-orch/issues/98)). As it stood: "G7
Throughput meets the budget, or the fallback ships", proved by: R5, R63: the report
`docs/sqlite-event-throughput.md`, with figures for macOS and native Linux hardware: the first run, kept with
its FAIL on macOS against the first budget; the revised measurement; and the path that ships. The budget this
line is read against is the one the owner accepts under AP13, and until the owner has accepted the numbers and
the results the line is unproven. A miss that selects no event path, by the owner's ruling of 2026-11-21,
proves neither half of the line and goes back to the owner.

## Verification

Filled in by the PR that completes the work. The platform of every row is Linux (CI, a Linux host or the
Linux release gate; record K of "Owner's answers"). No row is `env-limited` or `waived by owner` for want of a
macOS result: a macOS-only gap is an issue in the [After 2.0](https://github.com/yuxhuang/r-orch/milestone/20) milestone and not a row here.

| Id | Result (`pass` / `env-limited` / `waived by owner`) | Commit the check ran at | Platform |
|---|---|---|---|
| R1 | | | |

Defects found outside the requirements:
- ...
