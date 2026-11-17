# Implement bounded PR3 partial receipts

status: active
owner: senior engineer
date: 2026-11-17
spec: [partial receipts](../specs/2026-11-17-pr3-receipts.md)

## Goal and context

Unblock PR3 failure retention without treating a suite-only diagnostic as qualifying evidence. Frozen preparation is docs-only. The lead's bounded receipt decision controls; CI integration follows this immutable handoff.

## Design and source mapping

`gate.rs` adds mutually exclusive `--receipt` and `evidence verify-receipt`, rejecting unsupported tiers/parts before runtime. `report.rs` factors only the bounded manifest loader used by the unchanged strict verifier. New `receipt.rs` validates partial scope/source identity and derives complete/incomplete plus passed/failed/interrupted/refused diagnostic outcome from existing fields, logs and inventory. Successful partial commands explicitly allow only expected partial omission; unrelated nonqualification reasons remain nonzero. The tier retains the original result/bundle representation and manifest exporter.

`tier.rs` emits fixed progress on receipt start, suite return, target return and final export; `suite.rs` emits fixed build/test boundaries through a narrowly enabled Runner progress seam. No raw child log or path enters progress. The first message states final evidence is pending; export success is announced only after export. Graceful interruption follows existing finish/export; abrupt loss can leave only IN PROGRESS local state and no receipt. No signal/target ownership logic changes.

[ADR0016](../decisions/0016-partial-gate-receipts.md) distinguishes integrity from suite success and full qualification. Reject raw upload, translating exit3 to green, and general streaming redesign.

## Tasks

| Id | Task | Covers | Scope | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| T1 | Add failing CLI receipt controls before implementation | R1, R4 | `xtask/tests/gate_receipts.rs` | — | `cargo test -p xtask --test gate_receipts` | 3 |
| T2 | Implement receipt export, verification and bounded progress | R1–R5 | gate registry/report/tier/receipt/steps/suite | T1 | focused tests, `cargo test -p xtask`, fmt, affected clippy, crate-graph | 3 |
| T3 | Freeze source and scoped evidence for original CI owner/readers | R1–R5 | new spec/plan/ADR and private handoff | T2 | exact commit/tree/parent/status and raw ledger | 3 |

## Status and test plan

T1 implemented: original unsupported interface failed, then passed on the new source. T2 implemented and verified with focused receipt/evidence/tier controls and one xtask package run on macOS; final late-contention status delta has focused coverage after that package. Final lint receipts and exact immutable identity are in the author handoff. T3 freezes this implementation for the existing CI continuation and original independent readers; integration/review/CI/native acceptance remain pending. Test success/failure/export error, wrong identity/tamper/missing/unsafe artifact, redaction/control injection, interruption/loss and strict-verifier rejection. Reuse hermetic miniature fixtures and unchanged full-evidence controls. No tests removed or weakened. No full workspace suite; exact argv/status/count/platform retained privately. CI integration/native acceptance/TV50–55/cache and real cancellation remain outstanding.

## Rollback / compatibility and docs

Revert this scoped commit through normal review. No user persisted state or existing evidence format changes. Update only this supplement, ADR and decision index in this wave; CI owner carries procedure/integration docs. Existing approved goal spec/plan remain incomplete. No new approval, publication or live rules action.
