# CI fixture handshake correction

status: active
owner: senior engineer
date: 2026-11-16
spec: [requirements](../specs/2026-11-16-ci-fixture-handshake.md)

## Goal and context

Correct the twelve early fixture exits in PR 84 with a demonstrated cause and the
smallest test-only delta. The original CI log records exited children rather than
readiness timeouts, but discarded stderr prevents attributing their cause from that
log alone. The native setup used a private temporary parent; ordinary CI does not
promise that property. This environment difference is a hypothesis to test first.

## Design

Reproduce the exact original fixture on native Linux with retained separate channels
and actual child status. Exercise a shared temporary parent inside a private scratch
namespace. If the private-root assertion is the cause, supply an actually owned private
root rather than weakening that assertion or delaying readiness. Retain bounded,
labelled diagnostics on early exit using existing capture/formatting patterns. Keep
successful child output private. Do not introduce a diagnostics framework.

## Tasks

| Id | Task | Covers | Scope | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| T1 | Establish original child exit cause | R1 | Read-only original source and private evidence | — | Original fixture, retained status/stderr on native Linux | 84 |
| T2 | Correct private-root wiring and failure diagnostics; add regression | R1–R3 | `xtask/src/gate/target.rs`, `xtask/src/gate/steps.rs` | T1 | Focused target controls and diagnostic negative | 84 |
| T3 | Verify and freeze local correction | R1–R3 | Focused spec/plan | T2 | `cargo test --locked -p xtask`; `cargo fmt --all --check`; `cargo clippy --locked -p xtask --all-targets -- -D warnings` | 84 |

## Status

T1 established on native Linux: the original parent-root wiring supplies mode
1777 to an assertion requiring 0700. T2 implemented: pass the private control root,
retain failure-only labelled diagnostics with a 16 KiB read budget per stream,
and add shared-parent and deliberate early-exit regressions. Diagnostic files
are created exclusively as 0600; deliberately closed-output cases stay closed.
All twelve original affected test bodies are unchanged; no time bound changes.
T3 final package/lint checks run against the frozen commit and are recorded in
the PR 84 builder handoff. Independent reviews, new-head Mac folded acceptance
and Linux CI are subsequent lead-owned work; targeted controls do not replace them.

## Compatibility and documentation

Test-only change, no persisted product contract. No CHANGELOG entry is needed for
pure tests/CI under the repository documentation duties. Reverting restores the
fixture defect. Existing tiered spec/plan and gate comments remain unchanged.
