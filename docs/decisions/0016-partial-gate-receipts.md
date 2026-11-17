# 0016. Partial gate receipts verify diagnostics separately from qualification

- Status: proposed
- Date: 2026-11-17

## Context

PR3 runs the folded suite separately from static checks. The existing `--evidence` path and verifier deliberately require full qualifying evidence. Failure retention and by-name comparison require a safe shareable partial artifact, including when the suite fails. The lead approved the bounded correction; implementation and combined-source review are still required.

## Decision

Add `gate pr --part suite --receipt <dir>` and `gate evidence verify-receipt <dir> --commit <sha>`. Receipt and evidence flags conflict. Reuse the current private result layout, bounded redacted manifest exporter and inventory parser. The receipt verifier checks integrity and source/judge identity, then reports completeness and actual outcome as diagnostic only. Verifier exit0 means valid diagnostic bytes, including a failed or incomplete run; it never grants acceptance. Missing required files, unsafe structure or identity mismatch fail verification. Full `evidence verify` retains its strict predicates and rejects partial receipts.

A receipt-producing command returns0 only for successful requested steps with valid pinned judge/source, lease, lifecycle, named inventory and requested export. Expected omission of static checks does not hide other safety failures. Original signal and contention statuses take precedence. Emit only finite static phase messages before finish. Runner loss before final export leaves final evidence unavailable/incomplete; surviving progress and job conclusion cannot replace it.

## Consequences

CI can preserve verified redacted diagnostics while retaining the gate command failure independently. Integrity, suite outcome and full qualification remain separate. A valid receipt alone is never merge evidence; CI/native acceptance and owner-only rules transition remain separate obligations. No lifecycle recovery, general streaming, dependencies, accepted ADR bodies or product protocol changes. FS1/D45/option3 remain accepted, not fixed.
