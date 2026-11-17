# Ambient CI target resolution — implementation plan

status: active
owner: release engineer
date: 2026-11-17
spec: [requirements](../specs/2026-11-17-ambient-ci-build.md)

## Design and correction

Accept code F1/F2/F3 and SRE SR-A1/A2/A3. The previous build-first model was wrong: cargo test --workspace provisions debug/orch, and the relative target is read from crates/orch-infra at runtime. Historical author reports remain immutable; their absolute-target passes do not prove the failing CI configuration.

Set ambient's job env to CARGO_TARGET_DIR: ${{ github.workspace }}/target/ci-ambient. Remove Build ambient CLI and its incorrect comment. Keep the ordinary full workspace suite, event, failure, cache, permission, action, timeout and other target semantics. This removes the extra build cost raised by F3, without claiming measured hosted headroom.

Replace the two build-model tests and helpers. Pin the exact job expression and single target declaration, forbid ambient working-directory overrides and extra Cargo steps, and preserve the existing suite command. A private fixture places the artifact where Cargo provisions it under the workspace target and checks that same env-derived path using actual shell lookup from crates/orch-infra. The corrected expression resolves absolutely; the old relative value fails with the workspace artifact still present. No fake Cargo, build-order or missing-build premise remains. Native checks separately exercise actual Cargo and the unchanged tests.

## Tasks

| Id | Task | Covers | Scope | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| T1 | Correct this spec/plan before source | R1–R3 | this pair | adjudication | review accepted findings and retained holds | fix-forward |
| T2 | Replace wrong-model regression; show old workflow red | R1–R3 | xtask/tests/ci_workflow.rs | T1 | new regression against original 6802e79b workflow; planned negative | same |
| T3 | Correct target and remove redundant build | R1–R2 | .github/workflows/ci.yml | T2 | all ci_workflow, relative/override mutants, mirror, fmt, applicable clippy | same |
| T4 | Validate evaluated literal and freeze clean SHA | R3 | private evidence; this pair result text | T3 | Cargo test compilation provisions orch; native unchanged concurrent_index relative negative then literal absolute positive | same |

## Verification and stop conditions

- Red-on-original and relative-target controls are planned failures, labelled separately from unexpected failures. No unchanged retry-to-green; first unexpected failure stops and preserves evidence.
- Provision debug/orch using cargo test -p orch-cli --test daemon_cmd_control --no-run, under the evaluated literal corrected target, initially without that binary. This narrow compilation is setup for the infra-only run, not a claim that the full workspace suite needs a preceding build.
- Run unchanged concurrent_index with old relative target/ci-ambient: require exactly three missing-path failures despite the workspace binary. Then run with the corrected expression evaluated against the actual worktree root: require all three names passing.
- Audit remaining runtime CARGO_TARGET_DIR readers read-only; do not repair other jobs without evidence/scope.
- Original reviewers close only their own findings on the later combined head. Full native/pre-PR checks, natural hosted PR CI and actual main ambient acceptance remain held. No publication in this task.

## Status

T1–T4 focused author checks passed on macOS: original workflow regression failed at the package lookup; corrected ci_workflow 8/8, mirror controls, fmt and xtask all-targets clippy passed. Cargo test compilation provisioned the initially absent binary; native unchanged concurrent_index failed 0/3 under the planned old-relative control and passed 3/3 under the evaluated literal corrected value, with the same binary. Exact checked-source hashes, argv, statuses and frozen SHA are in the corrected task handoff. Earlier missing-build regression and absolute-only author proof are superseded as acceptance evidence, not erased. Combined-head reviews and deferred full native/pre-PR checks remain due. Main44051962151 remains failed; main/rules/PR3 acceptance and all inherited findings remain held.

## Rollback / compatibility

No data/protocol change. No rollback is executed or authorized here. If later withdrawal is authorized, a reviewed revert of the correction commit restores the known relative-target defect; prefer an established-cause fix-forward. Never rewrite history or delete a tag.

## Docs

Only this short supplement and spec. Parent goal and PR3 acceptance stay incomplete. Pure CI requires no changelog.
