# Bootstrap fixture PID publication

status: done

## Spec addendum

This PR2 fixture-only addendum implements SR-P1 under the approved
[tiered-checks spec](../specs/2026-11-14-tiered-checks.md) and
[plan](2026-11-14-tiered-checks.md). ADRs 0013–0015 remain unchanged.

The parked Cargo shim publishes `build-ready` only after a complete, closed PID
write. The final path must be absent while the write is empty or contains a
valid numeric prefix. Retrying until nonempty is insufficient: a prefix parses
successfully as a different PID. Write a sibling in the same private fixture
directory, then rename it over the final path, as `gate_recovery` already does.
This is publication atomicity, not a durability or descendant-cessation claim.

The pre-change event-controlled probe held the actual shim write after open and
after its first digit. It observed the final path containing empty text and a
valid numeric prefix, respectively; both states must become invisible.

Keep readiness, build release, signal delivery, stamp refusal, process-absence,
expiry rejection and lock-release assertions unchanged. Existing missing-file
readiness timeout, malformed-PID parse failure, command/reap bounds and retention
of unwinding or unresolved fixtures remain loud and bounded. A failed pre-rename
write must not publish readiness. No production code, dependency, CI or rule edit.

## Plan

| Task | Requirement | Owned paths | Dependency | Verify | PR |
|---|---|---|---|---|---|---|
| B1 | SR-P1 reproduction and contract | this addendum | pinned PR1 base | deterministic empty/prefix probe against actual shim | 2 |
| B2 | atomic publication | `xtask/tests/gate_bootstrap.rs` | B1 | discriminator fails with old publication, passes corrected | 2 |
| B3 | unchanged lifecycle and bounded failures | same test file | B2 | entire `gate_bootstrap` binary; bounded early-exit/missing/malformed controls; scoped fmt/clippy | 2 |

Extract only the existing parked-shim formatter so the discriminator executes
the exact fixture code. Intercept writes with events; intercept the final exec
boundary to avoid launching Cargo. Observe the real same-directory rename as
well as both pre-completion states. Deadlines guard hangs, never establish a
passing timing oracle. Preserve existing test bodies and all timeout values.
Retain negative receipts and source snapshots; no unchanged retry to green.

## Verification boundary

This task runs targeted hermetic native macOS checks only. It does not run the
workspace suite, a measured gate, Linux qualification or PR acceptance. SR-P2's
existing recovery-fixture 60-second drain cascade remains a named follow-up;
other historical findings and accepted FS1/D45/option3 risks remain scoped as
before. A task handoff is implementation evidence, not acceptance.
