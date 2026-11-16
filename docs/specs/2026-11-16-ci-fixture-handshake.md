# CI fixture handshake correction

status: approved
owner: senior engineer
date: 2026-11-16
plan: [implementation plan](../plans/2026-11-16-ci-fixture-handshake.md)

## Source

1. Owner: “Yes. But can these tests be changed with simple timing or delay or wait changes”.
2. The lead authorizes one bounded correction of the twelve PR 84 CI fixture exits,
   with a local commit for review before the coordinated push.

## Requirements

| Id | Requirement | Source | Priority | Acceptance check; platform |
|---|---|---|---|---|
| R1 | The fixture shall reach readiness with a shared temporary parent while retaining its private-root assertions. | 1–2 | must | Reproduce the original exit and run the affected target controls with a shared temporary parent; native Linux and macOS. |
| R2 | An early fixture exit shall retain its actual status and bounded, labelled failure diagnostics without successful child libtest output entering the parent stream. | 2 | must | Focused early-exit diagnostic regression and affected target controls; native Linux and macOS. |
| R3 | The correction shall preserve existing assertions, cancellation/capture/reap checks and timing bounds. | 1–2 | must | Diff inspection; `cargo test --locked -p xtask`; pinned `cargo fmt --all --check` and `cargo clippy --locked -p xtask --all-targets -- -D warnings`; native Linux and macOS. |

## Constraints and non-goals

Existing tiered-check specifications, plans and gate incident comments remain controlling.
No retries, serialization, timeout inflation, MCP shutdown-bound change, protocol or rule
change, new dependency, manual CI rerun, merge or release. A timing change is justified
only by demonstrated causality. Original failures and open conditions remain recorded.
The new head still requires independent review, Mac folded acceptance and Linux CI.

Security checklist:
- Exposure: test-only subprocesses; no new listeners.
- Bounds: existing readiness/teardown deadlines; bounded diagnostic reads.
- Files: exclusively created private fixture roots; diagnostic files remain inside them.
- Children: cleared environment, private HOME/global configuration, explicit tool PATH.
- Rendering: label every child diagnostic line; do not forward success frames.

## Verification environment

The builder uses isolated native Linux and macOS worktrees with pinned Rust 1.99.1,
private HOME/global configuration, fake agent CLIs, unique short scratch/target roots
and cooperative turnstile then lease. Diagnosis runs no full Linux or MCP suite.
The lead owns subsequent new-head acceptance and the single push.

## Verification

The original source reproduces all twelve early exits with a shared temporary
parent on native Linux. A one-line removal control restores that defect and fails
the new shared-parent regression with exit 101 and the private-root assertion.
The corrected focused controls pass on Linux. This is causal fixture evidence,
not attribution of any historical NM1 failure or full-gate acceptance.

| Id | Result at local freeze | Final check record | Platform |
|---|---|---|---|
| R1 | Focused cause/removal control demonstrated; final checks follow freeze | Exact SHA, commands, raw channels and statuses in the PR 84 builder handoff | Native Linux; macOS checks scheduled |
| R2 | Focused exit/status/label/budget regression passes | Same frozen-commit check record | Native Linux; macOS checks scheduled |
| R3 | Source diff preserves all twelve test bodies and timing bounds; package/lints follow freeze | Same frozen-commit check record | Native Linux and macOS |

The document stays approved while review and new-head acceptance remain pending.
Original GitHub and native Linux failures, the MCP follow-up and all inherited
open conditions remain intact. No timing/delay change is the honest fix: the
child has already exited after rejecting a nonprivate root.
