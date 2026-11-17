# Gate recovery PID publication implementation plan

status: active
owner: senior-eng
date: 2026-11-16
spec: [requirements](../specs/2026-11-16-gate-recovery-pidfile.md)

## Goal and design

Eliminate the final-path create-before-write interval observed in CI. Share the
actual holder script between qualification and a focused regression. Write the
PID into a sibling staging path, close the write, then atomically replace the
final path. Existence readiness retains its existing deadline and now means a
complete publication. Nonempty polling is rejected because it can accept a
numeric prefix. Bounded polling for new attempts is unnecessary.

The regression intercepts `Path.write_text` in a controlled Python thread running
the actual holder script. Events pause its real filesystem write immediately after
creation and after flushing a strict numeric prefix. Assert the final path is absent
at both points; release each event, join the holder and assert the final content is
the complete actual child PID. Always release events and join before cleanup.
The child uses its actual Popen/wait, with no signal to a PID read from a file.

## Tasks

| Id | Task | Covers | Scope | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| T1 | Add deterministic write-pause regression, run against old publication, then fix publication | R1, R2 | `xtask/tests/gate_recovery.rs` | — | exact regression fails old / passes corrected | 84 |
| T2 | Verify both existing qualification variants and unchanged oracles | R3 | same file, spec/plan | T1 | `cargo test --locked -p xtask --test gate_recovery`; scoped fmt/clippy | 84 |

- [ ] Retain original CI log/hash and exact base; create isolated worktree.
- [ ] Add and run deterministic regression before changing publication.
- [ ] Publish the closed sibling staging file atomically; run focused validation.
- [ ] Audit original assertions and run both existing filesystem variants.
- [ ] Commit only the three owned paths and freeze for existing reviewers.

## Review focus and limits

Empty publication, numeric prefix, completed content, failure cleanup and shared
filesystem coverage are covered by T1/T2. Staging and final paths must be siblings.
Existing no-inherit/unlock mutants, fd0, last-close and same-inode checks stay intact.
No production recovery claim, universal descendant/descriptor claim or Linux pass.
No full gate in this task. Existing reviewers consume the exact frozen commit;
the lead owns later acceptance and the one authorized push. No CHANGELOG is needed
for a pure test correction. Rollback changes no product data or public contract.
