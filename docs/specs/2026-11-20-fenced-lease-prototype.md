# Fenced lease throwaway prototype

status: approved
owner: senior engineer
date: 2026-11-20
plan: [prototype plan](../plans/2026-11-20-fenced-lease-prototype.md)

Approval scope: the owner explicitly requested this throwaway experiment below. This
approves only the experiment, not the proposed product corrections or phase 1 build.

## Source

From goal `goal_sMVh4SG`, task `tsk_jrPTVUS`; the goal wins on conflicts.

> 1. Prove the claim design on SQLite in a throwaway prototype before the engine depends on it.
> 2. One lease per task, enforced by the primary key of a `leases` table on (project, task). One active run per agent, enforced by a unique index over active leases on (project, agent).
> 3. Every run of a task gets an epoch from a per-task counter (`task_epochs`) that is never deleted or reset. A call that carries an older epoch is refused as superseded.
> 4. Starting a run is one transaction: insert the lease (fails if one exists), insert the run, set the task in progress, remove its retry entry, add an intent row to deliver the work.
> 5. A lease lasts 120 s and is renewed every 15 s; the holder gives up after 90 s without an acknowledged renewal, always shorter than the lease. A lost run uses no attempt.
> 6. Two claimants racing for one task: exactly one wins, the other gets a clean refusal.
> 7. A holder that stops renewing (process suspended, as when a laptop lid closes) and resumes after 50 s, 100 s and 200 s: it must never continue an attempt that another holder has taken, and its late result must be refused by epoch.
> 8. A wall-clock jump forward and backward during a lease.
> 9. A daemon restart: every lease counts as expired but is not reaped during a 120 s grace, so a holder can re-attach.
> 10. After a simulated database restore to an earlier snapshot, the next epoch for each task is set above the highest epoch in the restored runs and leases, so no epoch repeats.
> 11. Acceptance: a short report of what held and what did not, the prototype code outside the product crates, and concrete corrections for the spec if any rule above fails.
> 12. In release 1.1 the holder is still the daemon's own child process; the prototype shows the storage rules are right before workers use them in 1.5.

## Requirements

All test names refer to `prototypes/fenced-lease/src/tests.rs`; command:
`cargo test --manifest-path prototypes/fenced-lease/Cargo.toml --locked`.

| Id | Requirement | Source | Priority | Acceptance; platform |
|---|---|---|---|---|
| R1 | Competing connections get one claim; task and agent constraints reserve recovering leases too. | 2,6 | must | `task_race`, `agent_race`, `projects_are_independent`; macOS |
| R2 | Every claim commits all effects or none, including its epoch. | 3,4 | must | `claim_is_atomic_at_every_step`, `epoch_survives_task_deletion`; macOS |
| R3 | Suspended holders fail closed at 90 s, and replaced tokens cannot renew or finish. | 5,7 | must | `wake_after_50_100_200_seconds`, `renewal_send_time_bounds_authority`; macOS |
| R4 | Wall jumps cannot change authority. | 8 | must | `wall_jumps_do_not_change_lease`, `wall_clock_and_paused_clock_counterexamples`; macOS |
| R5 | Restart reserves expired leases for 120 s, rejects ordinary calls until explicit reattachment, and loses unrecovered runs without charging attempts. | 5,9 | must | `restart_grace_and_reattach`, `restart_after_grace_loses_without_attempt`; macOS |
| R6 | Restore demonstrates the snapshot-only counterexample and tests a proposed fresh-lineage correction, including an old live token. | 10,11 | must | `snapshot_maximum_repeats_a_discarded_epoch`, `restore_lineage_fences_discarded_future`, `restore_lineage_refuses_old_token_before_new_claim`; macOS |
| R7 | Prototype never enters product crates or runs agents, changes live state, or implements workers. | 1,11,12 | must | inspect diff; macOS |

## Non-goals and constraints

No product implementation, worker transport, real daemon, process killing, laptop
suspension, or clock adapter. All time and identity values are explicit model inputs.
SQLite must be real, WAL, STRICT, foreign keys on, NORMAL sync, BEGIN IMMEDIATE,
5 s busy timeout. Temporary databases only; no private fixture data.
Security checklist: no listener, shell or children; bounded literal test inputs;
temporary roots use tempfile permissions; no secrets or global configuration read.

## Verification environment

macOS, pinned Rust toolchain, bundled SQLite. Race tests use actual SQLite connections
on separate threads and barriers. Sleep, wall jumps, restart and restore use deterministic
clock injection and real database reopen/backup. Linux/real suspend integration is
future product acceptance, not a claim made by this experiment.

Since the owner's ruling of 2026-11-23 (record K of the storage spec) every later check of this prototype and
of the product it informs runs on Linux only, through 2.0. The results below were taken on macOS arm64 and stay
as recorded history. A macOS suspend is no longer a required check. The injected clock tests prove renewal and
deadline ordering only; the real suspend and resume of an isolated, authorized Linux host remains a product check
(spec R42, issue 110) and is pending. On Linux x86_64 the 21 tests passed at `200da1d4` (the first independent
review of the platform-scope documentation PR; the prototype is not a workspace member and CI does not run it).

Restore immediate redispatch assumes all previous holders are proven terminated;
the model does not enforce this. Two additional review tests isolate lineage with
reused session labels and stale reattachment after a second restart.

## Open questions

AP11 accepts compound restore lineage/counter fencing; AP12 keeps supervised 1.1
children alive across suspend and defers holder self-deadlines to 1.5. The accepted
[storage spec](2026-11-20-sqlite-storage.md) governs product behavior. Numeric no-reuse
cannot be proven from a snapshot alone. A real suspend proof was due under R42. Record K of the storage spec (2026-11-23) withdraws its macOS form; the
Linux check stays and is pending (issue 110).

## Verification

All 21 prototype checks passed on macOS at `0d5e91d`, byte-identical to `ec17542`.
Status stays approved while integrated repository gate/review/PR delivery are pending.
The older 19-case Linux container result at `bb44f34` does not qualify the later code.

| Id | Result | Commit | Platform |
|---|---|---|---|
| R1 | pass | 0d5e91d | macOS arm64 |
| R2 | pass | 0d5e91d | macOS arm64 |
| R3 | pass | 0d5e91d | macOS arm64 |
| R4 | pass | 0d5e91d | macOS arm64 |
| R5 | pass | 0d5e91d | macOS arm64 |
| R6 | pass (counterexample demonstrated; proposed correction tested) | 0d5e91d | macOS arm64 |
| R7 | pass (diff inspection) | 0d5e91d | macOS |
