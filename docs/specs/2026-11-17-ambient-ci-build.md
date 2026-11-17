# Ambient CI binary prerequisite

status: approved
owner: release engineer
date: 2026-11-17
plan: [implementation plan](../plans/2026-11-17-ambient-ci-build.md)

## Source

Bounded fix-forward authorized by the main-red decision for task tsk_ueqfAgR, under the owner's standing instruction:

1. “merge and continue next prs. repeat the loop; push the pr, wait for the ci run, merge the pr until all steps are done”.
2. Task repair scope: “Make ambient build required orch binary in SAME CARGO_TARGET_DIR before suite (cargo build -p orch-cli or justified minimal equivalent), preserve ambient event/coverage/failure semantics and isolation.”

This supplements the [PR3 spec](2026-11-17-pr3-ci.md). Run 44051962151 on main failed: all three concurrent_index tests required an absent target/ci-ambient/debug/orch. The tests derive their binary from CARGO_TARGET_DIR and deliberately refuse stale/foreign fallback.

## Requirements

| Id | Requirement | Source | Priority | Acceptance |
|---|---|---|---|---|
| R1 | Ambient shall build orch-cli in its job's target before running ordinary workspace tests; build failure shall stop execution. | item 2 | must | Executed workflow-shell regression with absent artifact, missing/reordered/wrong-target/masked-failure controls; macOS and Linux CI. |
| R2 | Ambient shall retain its main-push event predicate, target isolation, full workspace/no-fail-fast test argv and ordinary failure semantics. Other jobs remain unchanged. | item 2 | must | Existing ci_workflow contract plus actual shell execution; diff review. |
| R3 | The actual build shall provide the required binary to all three unchanged concurrent_index tests from an initially absent task-private target. | item 2 | must | `cargo build -p orch-cli`, then `cargo test -p orch-infra --test concurrent_index`; macOS now, main ambient Linux after reviewed publication. |

## Constraints and security

Only workflow, ci_workflow regression and this spec/plan change. No product/test-oracle weakening, remote-tools repair, retry, cache, action, permission, policy, release or runtime-config change. Pure CI needs no changelog. Existing main-red evidence remains failed. One heavy workload per host uses the configured cooperative lease, private HOME/global.yml, first-PATH AI blockers and umask 022; raw evidence starts as directories 0700/files 0600. Regression children clear their environment and execute only fixture cargo in a private directory. No event text is interpolated into shell.

## Verification environment

Native macOS author checks: focused ci_workflow, actual cold-target build plus three concurrent_index tests, fmt and applicable clippy. Full pre-PR suite is explicitly deferred to the post-review publication continuation. Hosted Linux full ambient green requires a later main event; focused success cannot establish it. No human question is required within this approved scope. Spec stays approved until acceptance completes.

## Verification

| Id | Result | Commit | Platform |
|---|---|---|---|
| R1–R2 | pass: eight focused workflow tests including four fault controls; downstream review/event acceptance pending | frozen author SHA in task handoff | macOS; Linux GitHub Actions pending |
| R3 | pass: absent-binary build plus all three unchanged concurrent_index tests; full ambient acceptance pending | frozen author SHA in task handoff | macOS; Linux GitHub Actions pending |
