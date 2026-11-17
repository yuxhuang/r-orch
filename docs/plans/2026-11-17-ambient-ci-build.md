# Ambient CI binary prerequisite — implementation plan

status: active
owner: release engineer
date: 2026-11-17
spec: [requirements](../specs/2026-11-17-ambient-ci-build.md)

## Design

Add a named Bash build step running `cargo build -p orch-cli` immediately before the existing ambient workspace-test step. Both inherit the same job-level CARGO_TARGET_DIR. Separate ordinary Actions steps stop on build failure; no shell status mapping, conditional build, cache dependency or suite weakening. The runtime dependency is already explicit in concurrent_index.rs. The existing single-line CI/Docker mirror continues to check the shared static and suite commands; the named build prerequisite has its own executed-shell regression and is not added to Docker's CI_STEPS in this scope.

Extend the existing fixed-indentation workflow test utilities to execute the actual ambient cargo run steps in order with Bash's Actions failure options and the job target. A fixture cargo creates the required artifact only for the expected build argv, refuses tests without that artifact, records exact argv/target, and can fail the build. Controls delete/reorder the build, redirect its target, or mask its failure. Assertions require both status and trace, so a skipped suite or ignored failed build cannot pass. Keep this fixture within the existing ci_workflow test; no new runner/controller.

## Tasks

| Id | Task | Covers | Scope | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| T1 | Write regression and observe old-workflow failure | R1–R2 | xtask/tests/ci_workflow.rs | — | focused ci_workflow regression | fix-forward |
| T2 | Add same-target build prerequisite | R1–R2 | .github/workflows/ci.yml | T1 | all ci_workflow tests, mutation controls | same |
| T3 | Validate actual missing-binary target and freeze | R3 | this spec/plan; no product changes | T2 | orch-cli build, three concurrent_index tests, fmt, applicable clippy | same |

## Test plan and review focus

- Missing/reordered build must fail before a successful suite; target override cannot satisfy the runtime dependency.
- Build exit statuses remain failures and prevent the suite, including a masked failure that would otherwise consume a stale artifact.
- Success must execute the full existing workspace argv once; event/target/cache isolation stays pinned by the existing contract.
- Real build begins with no debug/orch; focused tests keep their original bodies and expectations.
- Full pre-PR suite and Linux main-event acceptance remain deferred to reviewed publication; remote-tools diagnosis stays separate.

## Status

T1 and T2 verified on macOS: the original workflow failed the new regression, the repaired workflow passed all eight ci_workflow tests, and all four build fault controls were rejected. T3's local build and three unchanged concurrent_index tests passed from an absent-binary private target; fmt and xtask all-targets clippy passed. Immutable commit identity and raw author evidence travel in the task handoff. Full pre-PR and hosted main acceptance remain pending; this plan stays active. Approval is supplied by the explicit repair scope; no additional design approval is requested.

## Rollback / compatibility

No data/protocol change. Revert the fix-forward commit through a reviewed PR if needed; that restores the known missing-binary failure. Do not revert main, delete tags or retry unchanged CI. No deployment action in this authoring task.

## Docs

Only this short supplement and spec. Parent goal and PR3 acceptance stay incomplete. Pure CI requires no changelog.
