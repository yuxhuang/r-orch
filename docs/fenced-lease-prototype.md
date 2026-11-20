# Fenced lease prototype: entry-step findings

Date: 2026-11-20. Scope: goal `goal_sMVh4SG`, task `tsk_jrPTVUS`, entry step E.
This is a throwaway experiment, not a product storage implementation or worker protocol.

**Verdict:** SQLite enforces the claim and fencing transaction. The proposed rules need
corrections for suspend-aware time, delayed renewal acknowledgements, restart recovery,
and restore identity. In particular, snapshot maxima cannot guarantee numeric epoch
non-reuse. Product implementation still waits for the owner's phase 1 gate.

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
   Without that proof, quarantine dispatch for at least LEASE (120 s) after restore.
   The storage-only model enforces neither process termination nor quarantine.
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

F4, owner decision: applying this model to co-located 1.1 holders would stop every
holder after a whole-system sleep of at least 90 s. Leases become reapable at 120 s;
unrecovered runs are lost without charging an attempt. This is a visible change
from 1.0, not an adopted 1.1 policy. The spec author reports that 1.1 instead keeps
R42 (suspend ends no run), with holder deadlines deferred to 1.5. The owner must
approve the product rule. Same-boot recovery with a persisted boot identity is an
alternative to the model's conservative fresh session/120 s reservation.

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

Independent review passed with conditions at code `bb44f34` / docs `a552526`.
The review corrections add two tests (21 total), reuse the session label in the
existing restore test, and state restore/sleep limits; product behavior is unchanged.
The daemon-lineage mutant M1 and stale-session mutant M10 now each fail their named
new test (0 passed, 1 failed). Removing both lineage checks also fails the reused-session
discarded-future test with `expected Superseded, got Ok(())`.

The original fast attempt ended while waiting for host lease admission (exit 137);
final fast and pr at `a552526` were refused with exit 75, no steps run. Those are
not successful evidence. No target marker was removed. Fresh continuation evidence,
exact final head, gate results and PR status are recorded in the private task handoff.
No PR is opened while repository verification is incomplete. No product approval,
merge, tag or version bump is performed.
