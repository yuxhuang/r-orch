# Independent index timeout overlap

status: approved
owner: test-engineer
date: 2026-11-17
plan: [../plans/2026-11-17-index-timeout-overlap.md](../plans/2026-11-17-index-timeout-overlap.md)

## Source

Scoped supplement to approved goal R9 / plan node M. This does not propose new policy.
Verbatim owner source, goal shape item 4:

> Slowest tests: for each binary above 30 seconds, say where the time goes (fixed sleeps, timeouts, process starts, real cargo builds inside the test). Remove fixed waits where a controlled clock or an event can replace them without weakening the assertion. No test is deleted or weakened to save time; a test that cannot be made faster stays and is named.

Lead implementation direction (distinct from owner quotation): checkpoint tsk_MpinkK7
releases slice A after the real baseline: overlap only update/write/rebuild using the
same Tokio runtime, independent private roots and held locks; retain all original
assertions and deadlines. R9 and node M remain the governing authority. Baseline
prerequisite tsk_TecopmV recovered two passing native Mac samples; disturbed N=1.

## Requirements

| Id | Requirement | Source | Priority | Acceptance check; platform |
|---|---|---|---|---|
| R1 | The named timeout test shall execute update, write and rebuild independently on its existing runtime, retaining private roots/locks, 7s guard, >=5s real wait, TimedOut/error text, unchanged bytes, <=100ms cache read and post-release success. | owner item 4; lead slice A | must | `cargo test --locked -p orch-infra --test index_cross_process`; macOS; native Linux follow-up |
| R2 | One case failure shall allow the other joined cases to finish and release their local fixtures before the parent rethrows a panic. | lead slice A | must | Three separate disposable omitted-lock controls each fail `every writer must respect the external lock` after collecting all outcomes; macOS |
| R3 | All seven existing identities and default 20 rounds x 4 writers x 32 saves shall remain unchanged. | owner item 4 | must | Whole-target 5 passed/2 ignored; base-to-head diff and name comparison; macOS |

## Non-goals and constraints

No production, path, process, environment, dependency, clock or timeout mechanism
changes. No detached tasks. No PR4, publication or full/prePR campaign. Functional
checks establish no speedup: the performance owner compares frozen commits separately.
Security checklist: n/a for this scheduling-only delta; existing child entrypoints,
permissions and environment remain byte-identical.

## Verification environment

Pinned Rust 1.99.1; private HOME/TMPDIR/global.yml/fake-agent PATH and unique absolute
target under cooperative lease. Builder runs whole target, three mutants, fmt/clippy
and one approved fast iteration tier at the final commit. Independent code/SRE
readers and Mac/Linux performance evidence remain separate, outstanding work.

## Open questions

None within this approved slice.

## Verification

Builder macOS receipts are retained in the tsk_GTvjSeg handoff, which freezes the
commit identity and logs. Status remains approved: independent reviews and native
Linux/after measurements are outstanding. Local whole-target 5 passed/2 ignored,
three intended assertion failures, fmt and scoped clippy passed before commit.
The approved fast tier runs once at that final commit; its actual verdict is in the
handoff and cannot be inferred from the scoped checks. This is no timing result.
