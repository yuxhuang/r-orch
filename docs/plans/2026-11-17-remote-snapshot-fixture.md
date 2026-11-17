# Quiescent remote snapshot fixture — implementation plan

status: active
owner: senior engineer
date: 2026-11-17
spec: [requirements](../specs/2026-11-17-remote-snapshot-fixture.md)

## Goal and context

Make the remote snapshot test own its absent-status premise. The live peer manager writes Connecting/Backoff; the production RPC correctly reads the stored snapshot first. Keep the exact contract and quiesce only the requesting test fixture.

## Design

Add HarnessBuilder::without_peer_runtime, default false, rejecting an explicit listen address. Following normal Running bring-up, attach harness-owned directories before awaiting a testing-only Running peer drain. That drain takes the owned HostedPeer, spawns its existing shutdown into Running.loops, awaits and removes the joined handle, and reports join failure. Cancellation leaves the handle in Running so its existing Drop drain retains project/scratch ownership. A cfg(test, feature=testing) ownership query serves daemon unit tests only.

Add Fixture::without_peer_runtime using a shared private builder-start helper; ordinary Fixture::start stays live. Only remote_tests' fixture chooses the opt-out, before seeding. Keep the original fallback assertions and add absent-store checks before/after status. Then persist a deliberate offline Backoff with last_error, next_retry_at and attempts, demand that exact projection, and recheck list.

Use a held existing operation boundary and a one-poll handshake to prove the drain cannot finish early, plus a cancellation case proving the retained handle/guard. Harness ownership checks distinguish default live peer versus opted-out empty slot without timing. No replacement peer runtime, controller or snapshot handler change.

## Tasks

| Id | Task | Covers | Scope | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| T1 | Record source-grounded requirements/plan | all | this pair | approved adjudication + corrected ambient SHA | source read | fix-forward |
| T2 | Add opt-out/drain and deterministic lifecycle tests | R1,R2,R5 | daemon lifecycle.rs, testing/mod.rs | T1 | focused daemon tests; remove-opt-out and no-await mutants | same |
| T3 | Select quiescent MCP fixture and check both projections | R3,R4 | mcp/testing.rs, tools/remote_tests.rs | T2 | focused remote tests; wrong-fallback/ignore-snapshot mutants | same |
| T4 | Verify/freeze combined repair and report | all | private evidence; result text in this pair | T3 | feature checks, fmt, clippy; clean commit/tree/ancestry; predecessor-file hash equality | same |

## Test plan and stop conditions

One focused pass of affected modules plus feature-disabled/enabled crate checks, fmt and applicable all-target clippy under the configured turnstile then lease. Plan negative controls explicitly: remove builder opt-out branch; remove await from the peer drain; change fallback Connecting to Backoff; discard stored snapshots in the RPC. The latter two are temporary uncommitted negative-control edits only, restored immediately and byte-checked, never part of the frozen repair. Each must fail its named assertion; a build error or unrelated failure is not discrimination. No unchanged retry-to-green. First unexpected failure stops workloads and preserves all evidence for the lead.

## Status

T1 recorded before Rust edits. T2–T4 pending. Full native/pre-PR and natural hosted acceptance are deferred to the existing review/checkpoint chain, not waived.

## Rollback / compatibility and docs

No on-disk, wire or product behavior changes. An authorized future normal reviewed withdrawal can remove this test-only seam; no rollback/public action here. This spec/plan suffices for test-only behavior; no changelog, runbook or ADR change. Predecessor ambient files stay frozen. No new tasks/watchers.

## Open questions

None in this bounded scope.
