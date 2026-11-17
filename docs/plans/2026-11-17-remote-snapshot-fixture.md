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

Use a held existing operation boundary, an initial Pending poll, 16 scheduling yields and a second Pending poll before releasing the boundary, plus a cancellation case proving the retained handle/guard. The first poll alone cannot prove a join: a newly spawned handle is Pending before it runs. The second poll must reject a drain that yields then detaches without joining. Harness ownership checks distinguish default live peer versus opted-out empty slot without timing. Document that quiesce is not retryable after cancellation; callers must drop Running so the retained drain remains owned. No replacement peer runtime, controller or snapshot handler change.

## Tasks

| Id | Task | Covers | Scope | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| T1 | Record source-grounded requirements/plan | all | this pair | approved adjudication + corrected ambient SHA | source read | fix-forward |
| T2 | Add opt-out/drain and deterministic lifecycle tests | R1,R2,R5 | daemon lifecycle.rs, testing/mod.rs | T1 | focused daemon tests; remove-opt-out and no-await mutants | same |
| T3 | Select quiescent MCP fixture and check both projections | R3,R4 | mcp/testing.rs, tools/remote_tests.rs | T2 | focused remote tests; wrong-fallback/ignore-snapshot mutants | same |
| T4 | Verify/freeze combined repair and report | all | private evidence; result text in this pair | T3 | feature checks, fmt, clippy; clean commit/tree/ancestry; predecessor-file hash equality | same |

## Test plan and stop conditions

One focused pass of affected modules plus feature-disabled/enabled crate checks, fmt and applicable all-target clippy under the configured turnstile then lease. Plan negative controls explicitly: remove builder opt-out branch; remove await from the peer drain; change fallback Connecting to Backoff; discard stored snapshots in the RPC. The latter two are temporary uncommitted negative-control edits only, restored immediately and byte-checked, never part of the frozen repair. Each must fail its named assertion; a build error or unrelated failure is not discrimination. No unchanged retry-to-green. First unexpected failure stops workloads and preserves all evidence for the lead.

The bounded R-1/SR-C1 continuation preserves the original controls and failure evidence without repeating them. Reuse the original reviewer's probe-a survivor only after checking its source and mutation identity: it spawns/pushes shutdown, yields once, then pops the unjoined handle. Run that same planned mutant against the strengthened test and require the named second-poll assertion to fail; restore lifecycle.rs and compare hashes. Preflight the existing launcher path, syntax, argv, private resources and pin before any workload. At the corrected head run lifecycle, harness and remote modules once, plus cargo check -p orch-daemon --lib --features testing; cargo check -p orch-cli --lib; cargo fmt --all --check; and cargo clippy -p orch-daemon -p orch-cli --all-targets --features orch-daemon/testing -- -D warnings. Feature-off evidence may be reused only after establishing the covered implementation is byte-identical. No full/pre-PR run in this continuation.

## Status

T1–T3 implemented; original focused author tests and four planned mutants passed their intended oracles. T4 stopped on a launcher entry error before feature-on/CLI checks, fmt-check and clippy; that original handoff remains blocked. The authorized R-1/SR-C1 continuation updates this pair before Rust, strengthens one test and adds one cancellation documentation line, then supplies corrected-head source receipts and the yield-detach discriminator. The continuation handoff records their actual results; this plan does not infer success. Independent combined-head review and full native/pre-PR and natural hosted acceptance remain deferred to the existing review/checkpoint chain, not waived; status stays active.

## Rollback / compatibility and docs

No on-disk, wire or product behavior changes. An authorized future normal reviewed withdrawal can remove this test-only seam; no rollback/public action here. This spec/plan suffices for test-only behavior; no changelog, runbook or ADR change. Predecessor ambient files stay frozen. No new tasks/watchers.

## Open questions

None in this bounded scope.
