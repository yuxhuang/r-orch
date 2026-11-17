# Complete PID publication in gate recovery fixtures

status: approved
owner: senior-eng
date: 2026-11-16
plan: [implementation](../plans/2026-11-16-gate-recovery-pidfile.md)

## Source

1. Owner instruction, verbatim: > fix

The approved second bounded CI round limits this instruction to the PID publication
race in `xtask/tests/gate_recovery.rs`, based on PR 84 head `3f783285`.
The shared qualification helper serves both existing filesystem variants.

## Requirements

| Id | Requirement | Source | Priority | Acceptance check; platform |
|---|---|---|---|---|
| R1 | The holder shall expose the final PID path only after the complete child PID has been written and closed. | 1 | must | `holder_publishes_pid_only_after_complete_write`; macOS |
| R2 | A controlled empty-write and numeric-prefix interval shall fail with the old publication and pass with corrected publication. | 1 | must | Same regression, retained old-fail/new-pass outputs; macOS |
| R3 | Both original filesystem controls shall retain all marker, lock, inode, direct-process, stdin, last-close and mutant assertions and deadlines. | 1 | must | `cargo test --locked -p xtask --test gate_recovery`; macOS |

## Constraints and security

Only the named test file and these short spec/plan documents change. No dependency,
product behavior, retry, timeout inflation, assertion weakening or skipped case.
Keep existing 20-second readiness, 60-second drain and 5-second reap bounds.
No signal authority comes from diagnostic PID text. No socket or external service.
Fixture files live under private disposable directories. The staging PID file and
final PID file share a directory and filesystem. Children receive explicit isolated
environments; the regression starts only a finite local Python child and joins it.
No shell evaluation of untrusted text, real AI CLI, real HOME/config or secrets.

## Verification environment

Normal pinned Rust 1.99.1 commands on macOS under cooperative turnstile then lease,
close only; unique target, private HOME/global config/scratch, first-PATH AI blockers.
Linux CI and the new-head Mac folded gate belong to later tasks. This correction
supplies scoped evidence only, preserving D45, FS1, option 3 and prior open findings.
No full suite, push, merge, manual CI rerun, release, tag, version or rule change.

## Verification

Execution results and exact frozen source identity are retained in the task handoff.
Independent frozen-delta security/reliability review and new-head acceptance remain due.
