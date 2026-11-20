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
| R6 | Restore demonstrates the snapshot-only counterexample and tests a proposed fresh-lineage correction, including an old live token. | 10,11 | must | `snapshot_maximum_repeats_a_discarded_epoch`, `restore_lineage_fences_discarded_future`; macOS |
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

## Open questions

Owner gate must choose a restore fencing contract and approve the corrections in
[the report](../fenced-lease-prototype.md). Numeric no-reuse cannot be proven from a
snapshot alone. No correction here changes a product contract.

## Verification

Prototype checks passed at code commit `bb44f34`; status stays approved because
repository gate validation and PR delivery are incomplete.

| Id | Result | Commit | Platform |
|---|---|---|---|
| R1 | pass | bb44f34 | macOS arm64; native Linux arm64 container |
| R2 | pass | bb44f34 | macOS arm64; native Linux arm64 container |
| R3 | pass | bb44f34 | macOS arm64; native Linux arm64 container |
| R4 | pass | bb44f34 | macOS arm64; native Linux arm64 container |
| R5 | pass | bb44f34 | macOS arm64; native Linux arm64 container |
| R6 | pass (counterexample demonstrated; proposed correction tested) | bb44f34 | macOS arm64; native Linux arm64 container |
| R7 | pass (diff inspection) | bb44f34 | macOS |
