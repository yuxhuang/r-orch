# Bounded partial-suite diagnostic receipts

status: approved
owner: senior engineer
date: 2026-11-17
plan: [implementation plan](../plans/2026-11-17-pr3-receipts.md)

## Source

1. Owner standing direction: “merge and continue next prs. repeat the loop; push the pr, wait for the ci run, merge the pr until all steps are done”.
2. Lead bounded correction: “Choose the existing export machinery with an explicit partial-receipt interface.”
3. Lead boundary: “Failed/interrupted receipts may be valid diagnostic records but never pass acceptance.”

The lead's PR3 assessment authorizes this implementation within approved goal R7 and section 7 failure retention/by-name acceptance. It grants no CI, native, timing, publication or rule readiness. PR2's simplification does not apply.

## Requirements

| Id | Requirement | Source | Priority | Acceptance check; platform |
|---|---|---|---|---|
| R1 | Only `gate pr --part suite --receipt <dir>` shall produce partial receipts; evidence and receipt options conflict. | item 2 | must | `gate_receipts::receipt_interface_is_explicit_and_scoped`; macOS + Linux |
| R2 | Receipt export shall reuse bounded redaction, manifest integrity and identity checks, retaining suite logs and named inventory. | item 2 | must | `gate_receipts` export/identity/missing/tamper controls; macOS + Linux |
| R3 | Receipt execution shall return zero only when the requested suite and all lifecycle/source/lease predicates and export succeed; original safety/signal/contended statuses remain nonzero. | item 3 | must | receipt policy unit tests and miniature failure/export/interruption controls; macOS + Linux |
| R4 | `gate evidence verify-receipt <dir> --commit <sha>` shall report completeness and outcome as diagnostic only; full verification shall continue rejecting partial evidence. | items 2, 3 | must | `gate_receipts` verifier controls plus unchanged `gate_evidence`; macOS + Linux |
| R5 | Before final persistence, bounded static progress shall disclose that final evidence is pending; loss before export shall leave final receipt unavailable/incomplete. | item 3 | must | interruption/loss controls in `gate_receipts`; macOS + Linux |

## Non-goals

No CI workflow, general streaming/capture protocol, supervisor, parser replacement, lease/target recovery, product state format or qualifying verifier relaxation. No full-workspace/native acceptance in this author wave. FS1, D45 and option3 remain accepted risks, not fixes.

## Constraints

Execution exits: 0 means the requested suite, safety predicates and export succeeded; 1 means a failed suite/refusal/export or I/O error; 2 is usage; 3 is an otherwise completed nonqualifying/ENV-LIMITED run; 75 is lease contention; interruption preserves 128 + signal, including late export failure. Contention likewise retains 75 on late export failure. The verifier returns 0 for valid diagnostic integrity (including failed/incomplete outcome), 1 for rejection and 2 for usage. No verifier exit grants acceptance.

Reuse the current result/bundle format, parser and finite export budgets. Additive tooling CLI only. No dependencies, toolchain, manifests or accepted ADR bodies change.

Security checklist:
- Exposure: local CLI; only exporter-produced verified redacted paths may be shared.
- Bounds: existing 32-file, 64 MiB bundle and per-log limits; progress is a fixed finite set of static lines.
- Permissions: private directories 0700, files 0600 at creation under workload umask022.
- Children: existing hermetic environment, private HOME/global.yml and fake AI blockers.
- Rendering: progress contains no untrusted values; final rendering uses existing redaction/escaping. No child log is printed by progress and no shell parses receipt fields.

## Verification environment

Focused and package tests on macOS with pinned Rust1.99.1, private resources and existing turnstile then configured host lease. Linux CI, real cancellation/runner-loss semantics and native goal acceptance remain later obligations. Miniatures are diagnostics, never equivalence or performance evidence.

## Open questions

None for this bounded implementation; original independent reviewers judge the committed delta.

## Verification

| Id | Result | Commit the check ran at | Platform |
|---|---|---|---|
| R1 | pass: original interface red; scoped positive/negative controls | author source; commit binding in handoff | macOS arm64 |
| R2–R4 | pass: synthetic integrity/identity and actual miniature export/failure controls; strict verifier regressions | author source; commit binding in handoff | macOS arm64 |
| R5 | pass: direct inert worker TERM and pre-admission abrupt loss; real GitHub cancellation remains pending | author source; commit binding in handoff | macOS arm64 |

Implementation checks are scoped author evidence only. The success policy is tested deterministically; miniature production commands retain exit3 for private lease override and unstamped direct judge. A real configured/stamped positive and combined CI/native acceptance remain downstream. The once-run package passed before the final late-contention export-priority delta; that bounded delta and all receipt/evidence/tier controls passed a subsequent focused wave. No timing/equivalence claim is made.

Defect outside the runtime receipt requirements: the existing optional tier fixture recorder created permissive inner records under022 despite a private root. Originals are preserved; explicit private creation and an original-fails/new-passes filesystem control supply prospective correction. This does not rewrite the earlier failed capture evidence.
