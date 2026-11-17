# Tiered checks: developer documentation and rules (PR 6)

status: draft
owner: technical writer
date: 2026-11-17
plan: [implementation plan](../plans/2026-11-17-tiered-checks-documentation.md)

## Source

Supplements R10 of the [approved goal spec](2026-11-14-tiered-checks.md) (shape items 8 and 9) without completing the goal. Base: `main` at e02bc7f5704f951a251de85638a8d9b4200d85b8.

1. Goal shape 8, verbatim: “Team rules, changed by the owner's word at the approval gate and then written into AGENTS.md and the team's rule files: which tier a builder runs, which tier a reviewer may rely on, which tier gates a merge, one green CI execution per pull request instead of three, and the full cold gate once per goal. Until the owner says so, the existing rules stand.”
2. Goal shape 9, verbatim: “Documentation: AGENTS.md, the developer section of the README or RUNBOOK, and the comments in the gate script say what each tier proves and what it does not.”
3. Owner answers Q1–Q5 and the final plan approval, as quoted in the amendments table of the goal spec: one green Linux execution at the final PR head (Q1); a native Linux worker may replace GitHub's suite evidence, with GitHub's CI as the fallback when it or the Mac is down (Q2); warm per-worktree PR target, cold full gate (Q4); cooperative host-wide lease (Q5). Q3 (nextest) is superseded by item 5: nextest is not adopted.
4. Lead's PR 6 preparation instruction: separate approved policy from live GitHub enforcement; claim no timing, nextest, qualification or goal acceptance; keep owner-dependent wording marked.
5. Owner decision of 2026-11-17 (“Defer PR 4. Is it a blocker”), recorded in the goal's owner-reply of that date: PR 4 is deferred and cargo-nextest is not adopted, because its dependency audit found 23 advisories and 10 licence items outside the repository's allow-list. Sequential test binaries stay; the 20-run qualification on each platform and the at-least-2x nextest speedup are not delivered; no licence allowance, advisory ignore or tool change is authorized. It is a scope deferral, not a waiver of any other acceptance.

## Requirements

| Id | Requirement | Source | Priority | Acceptance check |
|---|---|---|---|---|
| R1 | The documents shall give each tier's actual command, flags, target, lease mode, steps and exit codes, as the frozen `xtask` source and `--plan` output show them. | item 2 | must | Every command and flag is found in `xtask/src/gate.rs`, `gate/tier.rs`, `gate/suite.rs` and `xtask/gate.sh`; the read-only commands in the plan's test section produce the quoted output. |
| R2 | The documents shall describe change-based selection: classes, fail-closed cases, the documentation-only path, the printed reasons, and what selection does not see. | item 2 | must | Quoted `gate select` output reproduced in a throwaway worktree; classes found in `gate_selection/classify.rs`. |
| R3 | AGENTS.md shall say which tier a builder runs, which evidence a reviewer may rely on and which does not count, which check gates a merge, and when the full gate runs. | item 1 | must | Read against the Source rows; each rule names its status: implemented, approved policy, or awaiting the owner's rules transition. |
| R4 | AGENTS.md shall keep the standing local pre-PR check and the live `check` and `test` requirements until the owner applies a transition, and shall not present a skipped workflow as satisfying a required check. | item 1 | must | Text compared with the live-rule statement in the lead's decision of 2026-11-17; no sentence waives a standing rule. |
| R5 | The documents shall state the stated limits of each protection: ambient configuration is not run by any tier, opt-in variables are refused, `real-home` may be partial, a child with its own environment is not seen, and a leftover process the gate did not start is not detected. They shall say not to remove `target/` while a killed gate's command may still run. | item 2 | must | Each limit traced to ADR 0013, ADR 0015 or the runbook; the `cargo clean` sentence matches the refusal text in `gate/report.rs`. |
| R6 | The documents shall describe the cooperative lease for what it is: a cooperative lock that no process is killed to free, not a barrier against a process that does not take it. | Q5 | must | Compared with `gate/lease.rs`, `xtask/gate.sh` and ADR 0015. |
| R7 | The documents shall claim no measured timing, no nextest or other parallel runner as available, no qualification campaign, no completed goal acceptance, and no applied owner rule. Time figures are marked targets, or are the owner's baseline measurement with its date and commit. | item 4 | must | Search of the changed files for each such claim; every number has a labelled source. |
| R8 | Proposed edits to the team's shared rule files and to the gate script's comments shall be prepared as diffs and proposed text outside the repository tree and shall not be applied. | item 4 | must | The handoff lists the rule paths, the sources and the exact text; no file outside the owned set is changed. |
| R9 | The documents shall record PR 4 as deferred and nextest as not adopted, with the audit reason (23 advisories, 10 licence items, per the owner's record), sequential test binaries retained, the 20-run qualification and the at-least-2x speedup as not delivered, and no exception authorized. | item 5 | must | The same four facts appear in `AGENTS.md`, `README.md` and `docs/RUNBOOK.md` and agree with the owner's record; no document describes nextest as available. |

## Non-goals

No change to production code, the gate shell, workflows, dependencies, other plans or ADRs, GitHub rules, or shared rules. No release, tag, version, push or PR. No documentation of nextest as available, and none of qualification or measured results that do not exist.

## Verification environment

Everything runs on a developer's macOS machine without building the workspace. Commands are checked against source, and the read-only commands (`gate --help`, `gate <tier> --plan`, `gate select`) were run with a bounded `xtask` build and a throwaway worktree. No tier, no suite and no test run was executed for this work; any fenced command that starts one is marked “not run”. The correction round of 2026-11-17 (the source review's M1–M6 and m1–m9) used static source reading, retained evidence and patch dry runs only; nothing was compiled or run.

## Open questions

Wording that depends on the owner is listed in the handoff, not guessed here: whether `gate pr` replaces the local CI-set pre-PR check after the rules transition; the exact merge sentence after the owner applies the `main` ruleset change; and the final text of the shared team rules. Approved policy (Q1, Q2, the unavailable-host answer, the final plan approval) is stated as approved and is not an open question. `--warm` is documented as implemented (full only, developer-only); its eventual fate is a design note for the code owner, not an owner question.

## Verification

| Id | Result (`pass` / `env-limited` / `waived by owner`) | Commit the check ran at | Platform |
|---|---|---|---|
| R1–R9 | pending independent fact re-check of the corrected commit | pending review head | macOS, static source checks only |
