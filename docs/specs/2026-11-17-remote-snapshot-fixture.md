# Quiescent remote snapshot fixture

status: approved
owner: senior engineer
date: 2026-11-17
plan: [implementation plan](../plans/2026-11-17-remote-snapshot-fixture.md)

## Source

1. Owner standing direction: “merge and continue next prs. repeat the loop; push the pr, wait for the ci run, merge the pr until all steps are done”.
2. Main-red adjudication explicitly authorizes: “Add a narrow opt-in test-only quiescent peer fixture using existing owned shutdown/drain before seeding any remote, default live-peer behavior unchanged.”
3. Same decision: “Keep exact Connecting fallback and add exact saved Backoff projection with stable list/schema/diagnostics assertions. Require deterministic opt-out/default lifecycle controls and mutants”.
4. Native-admission correction accepts code R-1: keep the operation boundary held through scheduling and a second Pending poll, discriminating the yield-then-detach mutant. Accept SR-C1: document that cancellation is not retryable and Running must be dropped.

This supplements the approved tiered-checks and PR3 specs. Main run 44051962151 remains failed. Triage recovered Backoff versus Connecting in an unchanged source tree: the harness starts a peer manager, which writes snapshots after the test seeds a remote. Maintenance-timer disablement does not disable that manager. No diagnostic rerun is needed.

## Requirements

| Id | Requirement | Source | Priority | Acceptance; platform |
|---|---|---|---|---|
| R1 | A fresh harness may opt out of peers by awaiting the existing owned peer shutdown before return; default startup retains its live peer. Contradictory explicit listen configuration is rejected. | 2 | must | testing module default/opt-out ownership test and listen rejection; macOS + Linux CI |
| R2 | The testing-only drain retains its join handle and project ownership on cancellation, joins before successful return and propagates join failure. After cancellation, callers must drop Running rather than retry quiesce. | 2,4 | must | lifecycle module controlled pending-drain and cancellation tests; method documentation; macOS + Linux CI |
| R3 | The MCP remote fixture alone opts out before adding a remote; absent snapshots remain absent around the status read and yield exact offline Connecting. | 3 | must | existing both_remote_tools_are_snapshot_projections_with_offline_fallbacks; macOS + Linux CI |
| R4 | An explicitly stored offline Backoff snapshot is projected with its diagnostic fields; list remains identical in both branches. Existing strict schema, configured-listen, null listener/controller, offline and shape assertions remain. | 3 | must | same remote test; wrong-fallback and ignore-snapshot planned mutants; macOS + Linux CI |
| R5 | Removing opt-out or skipping drain completion fails deterministic ownership/drain tests, including a spawned drain that yields once then detaches. The join test keeps the boundary held through scheduling and a second Pending poll; the initial poll alone is not join proof. Production startup/status, dependencies and fixture defaults remain unchanged. | 2–4 | must | planned source mutants plus feature-disabled/enabled crate checks and diff review; macOS + Linux CI |

## Constraints and security

Only the four assigned Rust modules and this spec/plan pair change. No sleeps, relaxed timeouts, ignored tests, alternative status assertions, dependency changes, runtime/controller or global serialization. Running additions are feature-gated to testing; lifecycle probes are unit tests, and MCP support is already cfg(test). Retain the predecessor ambient four files byte-for-byte.

This continuation changes only lifecycle.rs and this pair. All other remote files and the ambient four files remain byte-identical to the previously frozen combined source.

Security checklist:
- Exposure: use fresh unseeded private projects; opt-out shuts down existing peer tasks through HostedPeer::shutdown, before test seeding. No new listener or network implementation. Explicit listen + opt-out refuses.
- Bounds: existing operation boundary, join/drain and protocol/storage bounds; test waits use existing 20-second failure bounds and event/poll handshakes, never elapsed absence as proof.
- Files: existing harness private scratch/project paths and global.yml. Raw evidence directories0700/files0600 at creation; workload umask022.
- Children: existing turnstile then host lease, private HOME/global.yml, first-PATH17 AI blockers; no real AI CLI or user state.
- Rendering: no new untrusted text rendering or shell input.

## Verification environment and holds

Run focused daemon lifecycle/testing and MCP remote tests, feature-disabled/enabled checks, fmt and affected all-target clippy once at changed source on native macOS. Planned mutants are labelled expected failures, then restore exact checked source. Stop/preserve the first unexpected workload failure. Independent combined-head code/security/resilience reviews precede deferred full native/pre-PR workloads; natural hosted PR/main checks remain due. No publication or timing claim here. FS1/D45/option3 and inherited findings remain accepted-not-fixed. SEC-AMBIENT-1 historical exposure remains unfixed.

## Verification

| Id | Result | Commit | Platform |
|---|---|---|---|
| R1–R5 | original focused author tests/mutants passed, but required source checks stopped on a launcher entry error; the continuation handoff records actual corrected-head drain discrimination and source-check results; independent reviews and full acceptance remain due | exact source hashes/commit in handoff | macOS author; Linux CI pending |

## Open questions

None within this bounded correction. Spec remains approved until acceptance completes.
