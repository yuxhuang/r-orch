# Tiered checks: developer documentation and rules (PR 6): implementation plan

status: active
owner: technical writer
date: 2026-11-17
spec: [PR 6 requirements](../specs/2026-11-17-tiered-checks-documentation.md)

## Goal

`AGENTS.md`, `README.md` and `docs/RUNBOOK.md` describe the three gate tiers as they are implemented at `e02bc7f5704f951a251de85638a8d9b4200d85b8`, state the approved policy separately from it, and name what is still waiting for the owner. This supplements node N of the [goal plan](2026-11-14-tiered-checks.md) (PR 6) and does not complete the goal.

## Non-goals

No code, gate-shell, workflow, dependency, ADR or other plan change; no application of shared team rules or GitHub rules; no nextest, timing or qualification claim; no push, PR, release, tag or version.

## Design

Source of truth is the code, not the earlier prose. The interface text comes from `xtask/src/gate.rs` (arguments), `gate/tier.rs` (targets, lease mode, steps, qualifying reasons, exit codes), `gate/suite.rs` (fast selection and steps), `gate_selection/classify.rs` (classes), `gate/report.rs` (summary table, result paths) and `.github/workflows/ci.yml` (jobs). Every rule is tagged **Implemented**, **Approved policy**, **Draft policy** or **Awaiting the owner**, so a reader can tell enforced behaviour from agreed practice and from a pending GitHub change.

- `AGENTS.md`: new “Verification tiers” section replaces “Pre-PR check”'s gate paragraph and keeps the CI-set pre-PR check unchanged; workflow steps 3 and 5, the release step 1 and the implementer role follow it. Corrects two stale statements: `advisories` no longer “silently passes” without cargo-deny (it reports `ENV-LIMITED`, exit 3) and the cold gate needs 12 GiB (a hard check), not “about 12 GB”.
- `README.md`: the “Tiered verification work” text sat after the License section; it moves into “Development” as “Tiered checks”, shortened, with a real `gate select` output.
- `docs/RUNBOOK.md`: the section title “during PR1 repair” was stale and is renamed “Tiered gate”; adds a symptom, cause, fix table of verified refusal texts and a short procedure for judging a PR's evidence. The existing receipt, lease and recovery paragraphs are kept.

Rejected: one new central document (would duplicate and drift); deleting the CI-set pre-PR check (a standing rule that only the owner may change); documenting `pr` as the builder's pre-PR command (not approved).

## Tasks

| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| T1 | Spec and plan | R1–R8 | `docs/specs/2026-11-17-tiered-checks-documentation.md`, this file | — | Template headings present; plan status current | 6 |
| T2 | AGENTS.md verification tiers and workflow edits | R1–R7 | `AGENTS.md` | T1 | Source cross-check below; no sentence waives a standing rule | 6 |
| T3 | README developer section | R1, R2, R7 | `README.md` | T1 | Quoted command output reproduced | 6 |
| T4 | RUNBOOK tier section | R1, R5, R6 | `docs/RUNBOOK.md` | T1 | Each refusal text found in source or reproduced | 6 |
| T5 | Proposed team-rule diffs and gate-comment text | R8 | outside the repository, in the handoff folder | T2 | Paths and sources named; nothing applied | — |

## Status

| Item | State |
|---|---|
| T1–T4 | implemented on the PR 6 documentation branch; static checks only, independent fact check pending |
| T5 | prepared in the handoff folder; not applied |

## Test plan

Wording preparation only; no suite and no gate tier was run. Checks used, all read-only:

- `gate --help`, `gate fast|pr|full --plan`, `gate fast --warm --plan`, `gate fast --part suite --plan`, `gate lease status`, `gate select --base HEAD` on a throwaway worktree for documentation-only, one-crate, `Cargo.lock`, unknown-path and empty-change cases, using an `xtask` built into a throwaway target directory.
- Two direct refusals in the throwaway worktree: `gate fast --no-lease <reason>` and an inherited `ORCH_*` variable (not the wrapper, so their results say the judge is unstamped).
- Link targets and anchors in the three owned documents resolved by script; `git diff --check`.
- Not run: `./xtask/gate.sh` in any tier, `cargo test`, `cargo xtask crate-graph`, any suite, any CI run.

## Rollback / compatibility

Documentation only; revert the commit. No format, protocol or policy file changes; no live rule changes.

## Docs to update

This PR is the documentation. Other central documents (spec/plan of the goal, ADRs, `.github/PULL_REQUEST_TEMPLATE.md`, the gate script's header comment) are not edited here; the handoff lists the follow-ups.

## Open questions

Listed in the spec: the exact owner-dependent wording after the rules transition, the fate of `--warm`, and the final shared team-rule text.
