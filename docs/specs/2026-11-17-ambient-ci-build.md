# Ambient CI target resolution

status: approved
owner: release engineer
date: 2026-11-17
plan: [implementation plan](../plans/2026-11-17-ambient-ci-build.md)

## Source

Bounded correction authorized by the main-red adjudication for task tsk_zME8pYy, under the owner's standing instruction:

1. “merge and continue next prs. repeat the loop; push the pr, wait for the ci run, merge the pr until all steps are done”.
2. Adjudicated repair: “CARGO_TARGET_DIR: ${{ github.workspace }}/target/ci-ambient”; “Remove the redundant newly added build step”; replace the wrong-model regression with an absolute workspace-rooted target oracle and a relative-target negative control, including package-cwd resolution.

This supplements the [PR3 spec](2026-11-17-pr3-ci.md). Main run 44051962151 remains failed. The earlier missing-build diagnosis is withdrawn: workspace tests already provision debug/orch through orch-cli integration-test targets. The failure is relative path resolution: concurrent_index uses CARGO_TARGET_DIR verbatim from its package cwd, while Cargo writes the binary relative to the workspace root. The original author proof used an absolute target and is retained only as an absolute-target control; historical reports remain unchanged.

## Requirements

| Id | Requirement | Source | Priority | Acceptance |
|---|---|---|---|---|
| R1 | Ambient shall set its isolated target to the literal workspace-rooted expression in item 2, resolving to the same binary from the workspace and package cwd; remove the redundant explicit build. | item 2; F1/F3, SR-A1 | must | ci_workflow exact-expression/shape oracle and real filesystem lookup from package cwd; macOS and Linux CI. |
| R2 | Retain the main-push predicate, full ordinary workspace/no-fail-fast suite and producer failure status, caches, pins, permissions, timeout and every other job/target. | item 2 | must | Existing ci_workflow and Linux mirror checks; diff review. |
| R3 | Reject the original relative value for the actual package-cwd reason, and pass all three unchanged concurrent_index tests using the evaluated literal corrected CI target. | item 2; F2, SR-A2/A3 | must | Regression red on original workflow at 6802e79b and relative mutant; native relative negative then absolute positive on macOS, actual hosted main ambient after reviewed publication. |

## Constraints and security

Only workflow, ci_workflow regression and this spec/plan change. No concurrent_index body, remote fixture, gate.sh, linux.rs, cache, action, permission, policy or deployment change. Audit other runtime target readers read-only. The regression uses a private artifact marker and an actual shell filesystem lookup from the package cwd, without fake Cargo or a claim to simulate compilation. Native proof separately provisions the binary using Cargo test compilation; no preceding cargo build is required by workspace testing. No event data is interpolated into shell source.

One heavy workload per host takes configured turnstile then lease, with private HOME/global.yml and first-PATH17 AI blockers; workload umask022, raw evidence directories0700/files0600 at creation. Preserve SEC-AMBIENT-1's historical0644 exposure and the security reviewer's optional setup failure; neither is retroactively fixed. FS1/D45/option3 remain accepted-not-fixed.

## Verification environment and hold

Focused native macOS: ci_workflow, mirror, fmt, applicable xtask clippy, Cargo-test binary provisioning and unchanged concurrent_index negative/positive controls. Full native/pre-PR verification is deferred until combined final-head reviews. Hosted Linux main acceptance requires a later natural main event, including all three concurrent_index names; PR green alone cannot establish ambient. No performance, whole-suite or readiness claim follows from focused checks. Spec stays approved until acceptance completes.

## Verification

| Id | Result | Commit | Platform |
|---|---|---|---|
| R1–R2 | pass locally: ci_workflow 8/8, both mirror checks, fmt and applicable clippy; independent delta review pending | corrected SHA and checked-source hashes in task handoff | macOS; Linux GitHub Actions pending |
| R3 | pass locally: original workflow regression red at package lookup; native relative 0/3, evaluated-literal absolute 3/3 with unchanged binary provisioned by Cargo test compilation | corrected SHA and checked-source hashes in task handoff | macOS; hosted main pending |
