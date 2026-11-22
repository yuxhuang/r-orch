# Fenced lease prototype: entry-step findings

Date: 2026-11-20. Scope: goal `goal_sMVh4SG`, task `tsk_jrPTVUS`, entry step E.
This is a throwaway experiment, not a product storage implementation or worker protocol.

**Verdict:** SQLite enforces the claim and fencing transaction. The proposed rules need
corrections for suspend-aware time, delayed renewal acknowledgements, restart recovery,
and restore identity. In particular, snapshot maxima cannot guarantee numeric epoch
non-reuse. The owner has since accepted compound lineage/counter fencing (AP11)
and a separate 1.1 suspend policy (AP12); this experiment does not implement either
product path. See the [storage spec](specs/2026-11-20-sqlite-storage.md).

## Reproduce

From the repository root, with the pinned Rust 1.99.1 toolchain:

```text
cargo test --manifest-path prototypes/fenced-lease/Cargo.toml --locked
cargo fmt --manifest-path prototypes/fenced-lease/Cargo.toml --check
cargo clippy --manifest-path prototypes/fenced-lease/Cargo.toml --all-targets --locked -- -D warnings
cargo deny --manifest-path prototypes/fenced-lease/Cargo.toml --config deny.toml check
```

[Code](../prototypes/fenced-lease/src/lib.rs), [schema](../prototypes/fenced-lease/src/schema.sql),
[holder](../prototypes/fenced-lease/src/holder.rs), [tests](../prototypes/fenced-lease/src/tests.rs),
[spec](specs/2026-11-20-fenced-lease-prototype.md),
[plan](plans/2026-11-20-fenced-lease-prototype.md),
[proposed ADR](decisions/0018-fenced-lease-prototype.md).

An independent Cargo workspace pins rusqlite 0.41.1 with bundled/backup/cache and
no default features; libsqlite3-sys 0.39.1 bundles SQLite 3.54.1. No product manifests
change. The entry-step C audit chose this library; this workspace repeats cargo-deny
with the unchanged repository policy. Result: advisories/bans/licenses/sources OK;
11 warnings are unmatched policy allowances/ignored advisories because this small
graph does not contain the product's dependencies. No policy was weakened.

Every database is in a tempfile root. WAL, NORMAL synchronous, foreign keys, STRICT
tables, BEGIN IMMEDIATE and 5 s busy timeout are asserted. Two racing connections use
separate threads and a barrier; no product daemon, real agent CLI, home config, private
data, system clock adjustment or laptop suspension is used. Seconds and generation
identities are deterministic inputs. This isolates storage semantics from OS adapters.

## What held

| Scenario | Observed result | Named test |
|---|---|---|
| Two agents, one task | Exactly one winner and one TaskBusy; 32 independent races | `task_race` |
| One agent, two tasks | Exactly one winner and one AgentBusy; 32 independent races | `agent_race` |
| Same IDs across projects | Independent claims | `projects_are_independent` |
| Claim transaction | Lease, run, in-progress task, retry removal, delivery intent and epoch commit together; all six failpoints and an actual SQLite statement error roll back | `claim_commits_all_effects`, `claim_is_atomic_at_every_step`, `sqlite_statement_failure_rolls_back_the_claim` |
| Wake at 50 s | Still within 90 s; another claimant refused; authority renewal succeeds before work resumes | `wake_after_50_100_200_seconds` |
| Wake at 100 s | Holder stops; lease remains reserved until 120 s; replacement then uses epoch 2 | same |
| Wake at 200 s | Replacement owns epoch 2; old holder cannot work; old renewal/result refused as Superseded | same |
| UTC forward/backward | No authority effect when elapsed time is used; deliberately naive UTC variant gives premature takeover or delayed expiry | `wall_jumps_do_not_change_lease`, `wall_clock_and_paused_clock_counterexamples` |
| Restart, new clock origin | Lease is recovering, ordinary writes refused, task and agent reserved through t<120; reattach at119 preserves run/epoch and changes session | `restart_grace_and_reattach` |
| Grace boundary | Reattach at120 refused; claim at120 reaps and replaces; loss charges no attempt, accepted failure charges one | `restart_after_grace_loses_without_attempt` |
| Permanent task counter | Survives task/run deletion; delete/reset refused; overflow rolls back | `epoch_survives_task_deletion`, `exhausted_counter_and_clock_overflow_refuse_cleanly` |
| Restore repair | Next counter exceeds each task's restored run, lease and counter maximum, including deleted-task counters | `restored_counters_include_runs_leases_and_tombstones` |
| Fresh restore lineage | Old snapshot and discarded-future tokens cannot renew, finish or reattach, even with equal task/run/agent/numeric epoch | `restore_lineage_fences_discarded_future` |

At code commit `bb44f34`, all 19 tests passed on macOS arm64 and native Linux arm64
(Docker, rust:1.99.1-bookworm, network disabled), 0 failed on each; the races exercise 64 competing SQLite claim pairs per suite. Standalone fmt and clippy passed. The first test was observed
red against the unimplemented claim scaffold (0 passed, 1 failed), then green.
A negative control removed the renewal/result authority check: the wake test failed
with `expected Superseded, got Ok(())`; restoring the checks returned 19/19 green.
An initial backup API compile error and a constant-assertion clippy failure were fixed;
the corrected commands above passed. Real OS suspend is not measured here.

## Corrections for the phase 1 spec

1. **Authority time must include suspend, and must not be UTC.** Use daemon-local
   suspend-inclusive monotonic elapsed time and a new session/clock domain on every
   daemon start. Use the corresponding clock at the holder. On Linux, ordinary
   CLOCK_MONOTONIC excludes system suspend; CLOCK_BOOTTIME includes it
   ([Linux man-pages](https://www.man7.org/linux/man-pages/man2/clock_gettime.2.html)).
   On macOS, mach_continuous_time includes sleep, while mach_absolute_time does not
   ([Apple](https://developer.apple.com/documentation/kernel/1646199-mach_continuous_time)).
   Do not assume Rust Instant or a generic monotonic clock has the required semantics.
   A process suspension while the OS stays awake and full system sleep are distinct.

2. **Start the 90 s holder deadline at request send time for the last acknowledged
   grant, including the initial claim.** A response received at80 for a renewal sent
   at15/committed at16 must stop the holder at105, before daemon expiry136. Starting
   at acknowledgement receipt would allow work until170, overlapping takeover.
   Send renewals at least every15; compare elapsed time before every work/result
   boundary and on wake. After90 without a timely ACK the holder irreversibly stops;
   late ACK or reattachment does not revive that attempt. A resumed holder must
   validate current authority before resuming. See `renewal_send_time_bounds_authority`
   and `reattach_does_not_revive_a_stopped_holder`.

3. **Define restart expiry separately from reaping.** Before accepting operations,
   under the daemon's exclusive data-directory lock, atomically set a fresh daemon
   session and mark existing leases recovering with a grace deadline of startup+120.
   During grace they reserve both task and agent but cannot renew/submit results until
   explicit reattachment. Reattach validates full identity, keeps run/epoch and returns
   a current-session token with a new120 s lease. At t>=120 unrecovered leases become
   lost without charging an attempt. Old session tokens remain refused after reattach.
   Repeated crashes can extend reservation; this trades availability for safe recovery.

4. **Snapshot repair does not prevent global numeric reuse.** Concrete counterexample:
   snapshot contains epoch1; live database issues2; restore snapshot; scanning its
   runs/leases/counter yields1, so the next epoch is2 again. This is demonstrated by
   `snapshot_maximum_repeats_a_discarded_epoch`. Never claim otherwise. Recommend a
   fresh collision-resistant restore lineage in every fenced identity:
   `(restore_lineage, project, task, epoch)`, plus run/agent and daemon session checks.
   Ordinary restart retains lineage; restore invalidates old leases instead of allowing
   grace reattachment. Immediate redispatch requires the daemon stopped and every
   previous holder proven terminated, including holders from the discarded future.
   In this cooperative model only, a quarantine of at least LEASE (120 s) can
   replace termination proof if every prior holder enforces its 90 s self-deadline
   using suspend-inclusive time before any work boundary. Waiting alone proves
   nothing about 1.1 children, which have no such self-deadline. Product restore
   must retain stopped-daemon and actual-holder termination proof, including
   discarded-future holders. The storage-only model enforces neither process
   termination nor quarantine.
   Lineage is the durable fence across restarts; session fences liveness/reattachment.
   Tests reuse a session label across restore to isolate lineage, including before
   a replacement lease exists. Repair counters to max(restored counter, runs, leases), then
   increment transactionally on claim. The full fence is never reissued although the
   numeric suffix can repeat. If the owner instead requires globally increasing numeric
   epochs across arbitrary rollback, retain a high-water mark outside the restored
   image, with its own durability/restore contract. A database-only scan cannot do it.

The partial agent index includes recovering leases; expired rows are reaped in the
claim transaction before insertion. Expiry is not a time-dependent index predicate.
The task counter has no cascading task FK. Each accepted mutation validates authority
inside the same BEGIN IMMEDIATE transaction as its effects; an out-of-transaction
preflight is insufficient ([SQLite transaction documentation](https://www.sqlite.org/lang_transaction.html)).

## Independent review corrections

F1: correction 4 and ADR 0018 now state the stopped-daemon/all-holders-terminated
precondition for immediate redispatch, with a 120 s quarantine alternative.
F2: `restore_lineage_fences_discarded_future` reuses the session label;
`restore_lineage_refuses_old_token_before_new_claim` isolates the daemon-lineage
check before any new lease exists. F3: `stale_copy_cannot_reattach_after_second_restart`
refuses a session-1 copy while permitting the legitimate session-2 token.

F4: applying this model to co-located 1.1 holders would stop every holder after a
whole-system sleep of at least 90 s. Leases become reapable at 120 s; unrecovered
runs are lost without charging an attempt. This is an experimental consequence,
not the adopted 1.1 policy. The owner accepted AP12: keep R42 (suspend ends no run),
renew still-supervised children on resume before expiry, and defer holder self-deadlines
to workers in 1.5. Real hardware suspend proof remains due under R42.

N1: the 120 s quarantine alternative assumes every old holder has the model's
90 s self-deadline. It does not justify 1.1 restore redispatch. Separate process
controls on macOS prove cooperative SIGTERM exit and resistant-shell SIGKILL exit,
and a launch/daemon-stop control reaches tracked-work idle. They do not implement
restore-to-kill wiring, prove termination of every discarded-future holder, or
contain arbitrary descendants. N2: the before-new-claim lineage test pins the
`Superseded` classification; the individual lineage checks are redundant for
acceptance safety. N3: the ADR's stray leading whitespace is corrected.

F6–F8 limits: a stopped holder must not reattach; this model has no release operation,
so mistaken reattachment costs liveness until expiry. Finished-run intents remain
pending because delivery is not modelled; product finish must complete intents in
its transaction. Product open/restart must be atomic before accepting operations;
`Store::connect` alone is not that startup boundary. These are not product implementations.
This standalone workspace is frozen experiment evidence; root CI does not maintain it.

## Limits and product gate

The model assumes its elapsed clock advances correctly and both participants' clocks
have adequate rate bounds for the30 s margin. It does not implement a clock adapter,
a heartbeat thread, a transport, a data-directory lock, crash/power-loss durability,
authenticated holder identity, child-process termination or intent delivery. A fixed
lineage/session is used only for initial test creation; restore/start identities are
injected labels, not a production uniqueness generator. A future adapter must supply
fresh identities and prove suspend behavior before this is used in an engine.

Storage fencing refuses stale database effects; it cannot stop an arbitrary agent
child from writing external files immediately after wake. In1.1 the daemon must own
and stop its child and route accepted results through the fence. In1.5 worker execution
and external side effects need their own fencing/cooperation contract. The literal
claim that an uncooperative process can never execute after supersession is not proven
by a lease table. No worker feature is built in this task.

The prototype's reaper scans all leases and the test setup API is intentionally small;
neither is a proposed production writer implementation. Root cargo/gate commands do
not include this standalone workspace: its explicit commands above are required too.
Repository fast/pr gate evidence and PR delivery status are recorded in the task handoff.

## Repository delivery status

The prototype at `0d5e91d` (base `a8dc840d`, accepted lock-inode correction PR #103)
is byte-identical to the reviewed `ec17542` prototype. On macOS arm64, all 21 named
cases passed, with standalone fmt, Clippy and cargo-deny exit 0. Sleep/wake at
50/100/200 s uses injected elapsed time; no physical suspend/resume ran. Historical
19-case evidence at `bb44f34` on macOS and Linux above covers that earlier code only.
Root CI does not include this standalone workspace.

Earlier independent review passed with conditions at code `bb44f34` / docs `a552526`.
The corrections add two tests, reuse the session label in the existing restore test,
and state restore/sleep limits. The daemon-lineage mutant M1 and stale-session mutant
M10 each fail their named new test (0 passed, 1 failed). Removing both lineage checks
also fails the reused-session discarded-future test with
`expected Superseded, got Ok(())`.

Historical repository failures remain evidence: the first attempt ended while waiting
for admission (exit 137); fast and pr at `a552526` were refused (exit 75, no steps).
Fast at `ec17542` recorded 7,539 passed, 4 failed, 7 ignored. At `0d5e91d`, all four
previously failing names passed focused checks and exactly once in development fast:
7,568 passed, 0 failed, 7 ignored, eight internal steps passed. Those later passes
do not establish the cause of the old failures. Fast remains local iteration evidence.

Final integrated-head PR-tier evidence, independent review and Linux CI must be
recorded in the delivery handoff before publication. No merge, tag, version bump or
new product behavior is supplied by this experiment.
