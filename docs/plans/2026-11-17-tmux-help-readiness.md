# Tmux help content readiness

status: active
owner: test-engineer
date: 2026-11-17
spec: [../specs/2026-11-17-tmux-help-readiness.md](../specs/2026-11-17-tmux-help-readiness.md)

## Goal

The tmux table test consumes complete required help content from one capture before asserting it, and deterministic controls reject permanent omission within the unchanged deadline.

## Non-goals

Production changes, additional acceptance campaigns, full local gates, publication and transport-causation claims.

## Context

The 120×40 branch currently accepts the title alone before asserting the disclosure. The existing 80×24 branch already waits for eight footer lines. The observed failure proves incomplete readiness, while a partially applied terminal frame remains a hypothesis.

## Design

Move the eight existing footer strings into a test-local constant and share a title-plus-all-lines predicate between both sizes. Preserve the original assertion expressions and the 80-column bound. Extract the existing polling loop into a test-local function with capture, clock and poll callbacks; the real tmux wrapper retains its capture command, Instant clock, twenty-second deadline and 100 ms interval verbatim. Synthetic callbacks exercise the same loop without sleeps or processes.

Balanced stance favors this small test-local seam over production instrumentation or a new helper module. Controls cover immediate completeness, prefix then complete, permanently absent disclosure, each footer line and each disclosure sentence independently missing, absent title, cross-capture fragments and capture-error propagation. Temporarily restore the old title-only predicate to prove regression sensitivity. Synthetic controls do not establish tmux scheduling causation.

## Tasks

| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| T1 | Add deterministic controls and minimal polling seam; preserve the old predicate for the red check, then require complete content at both sizes | R1–R5 | `crates/orch-cli/tests/tui_tmux_qol2_tables.rs` | — | `cargo test -p orch-cli --test tui_tmux_qol2_tables help_readiness_` | one bounded repair |
| T2 | Verify formatting and one complete focused target with explicit CLI provisioning; freeze and hand off | R1–R5 | Same test file; this plan and its spec | T1 | `cargo fmt --all --check`; `cargo build -p orch-cli --bin orch`; `cargo test -p orch-cli --test tui_tmux_qol2_tables` | same |

## Status

| Item | State (implemented / verified on <platform> / pending) |
|---|---|
| T1 | implemented and verified on macOS: final eight deterministic controls; mutants rejected |
| T2 | verified on macOS: formatting, explicit CLI build and one tmux target; independent reviews and Linux acceptance pending |

## Test plan

Run deterministic controls against the old predicate first, preserve their failures, then run against the repair. Use a fixed fake clock advancing only at poll boundaries; every missing-content case must panic at twenty seconds with the last capture. Verify capture panics propagate. Run formatting and build the real CLI explicitly in a dedicated target before one execution of the entire named tmux target. The target retains both 120×40 and 80×24 checks. Stop on persistent real content failure and route it to the lead.

The final review added one sentence-omission control after the single real target run; only controls changed, all eight final controls passed, and the original scenarios/polling/readiness code stayed byte-identical. The target was not repeated.

The owner exception defers workspace clippy, the workspace suite, crate-graph, cargo-deny and cold/full gates. No baseline, Linux/multi-host campaign, new receipts campaign or workflow rerun. Monitor owns future Linux CI and main acceptance. Native terminal profiles, native clipboard and browser checks do not apply to this test-only repair.

## Rollback / compatibility

Revert the single repair commit. No on-disk, CLI, peer, production or user-visible behavior changes.

## Docs to update

Only the matching spec and this plan. Pure test changes need no CHANGELOG, README, RUNBOOK, ARCHITECTURE or ADR update.

## Open questions

None; unresolved transport behavior is reported rather than repaired in this task.
