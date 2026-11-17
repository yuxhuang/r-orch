# Independent index timeout overlap

status: active
owner: test-engineer
date: 2026-11-17
spec: [../specs/2026-11-17-index-timeout-overlap.md](../specs/2026-11-17-index-timeout-overlap.md)

## Goal and context

Implement only approved goal R9 / node M slice A after the recovered baseline.
The existing serial test imposes three real lock deadlines on independent fixtures.
Overlapping them is a source model, not measured savings; whole-target stress may
still dominate. Source and lead authority are distinguished in the spec.

## Design

Extract the original loop body into a nested `async fn case(operation: &str)`.
Join exactly update/write/rebuild with `tokio::join!` on the unchanged test runtime.
Use existing futures `FutureExt::catch_unwind` with `AssertUnwindSafe` around each
one-shot case: a panic drops that case's owned lock and temp root, while siblings
finish. Collect all results, then resume the first panic. A caught future is never
polled again. No spawned/detached task or production API change is introduced.
Keep every original case assertion and the rest of the file unchanged.
Serial execution retains unnecessary independent waits; spawned tasks introduce
lifecycle ownership with no benefit here; a paused clock would weaken the real
lock deadline oracle. Those alternatives are rejected for this slice.

## Tasks

| Id | Task | Covers | Scope | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| A | Extract/join cases and retain original oracles | R1-R3 | `crates/orch-infra/tests/index_cross_process.rs`, this spec/plan pair | actual tsk_TecopmV baseline | `cargo test --locked -p orch-infra --test index_cross_process`; three omitted-lock controls; `cargo fmt --all --check`; `cargo clippy --locked -p orch-infra --test index_cross_process -- -D warnings`; `./xtask/gate.sh fast --base e02bc7f5704f951a251de85638a8d9b4200d85b8` once at final commit | PR5 slice A, publication held |

## Status

| Item | State |
|---|---|
| Baseline prerequisite | Verified from actual passing native Mac samples; disturbed N=1 |
| A | Implemented; whole target, three controls, fmt and scoped clippy verified on macOS; final-commit fast outcome recorded in frozen builder handoff |
| Independent readers / frozen after measurement | Pending existing downstream tasks |

## Test plan and review focus

- Preserve the error/real-time/publication/cache/recovery assertions verbatim.
- Omit one held lock per disposable test copy; collect all three results before
  rethrow and require the expected assertion failure (never a compiler failure).
- Restore exact source after controls; whole target keeps 5 parents/2 ignored
  entrypoints and default 20x4x32 workload. No failed check is retried to green.
- Run the scoped fmt/clippy checks, commit only owned paths, then run approved
  fast tier once at the frozen commit; preserve any failure and stop acceptance.

## Rollback / compatibility and docs

Revert the isolated test/spec/plan commit. No data, protocol or product change;
no central docs or changelog update is needed. This pair mirrors the approved
slice requirements into repository docs. Raw receipts stay in the private handoff.

## Open questions

None for implementation; measurement and independent review remain due.
