# 0009. Remove parity validation and tooling

- Status: accepted
- Date: 2026-10-18

## Context

[0002](0002-parity-is-no-longer-a-goal.md) dropped parity with the TypeScript original `oxgeneral/ORCH` as a
requirement but kept the machinery: `docs/PARITY.md` and `docs/parity/`, `cargo xtask parity` and
`lint-scenarios`, the `fixtures/scenarios/` corpus, the vendored oracle under `parity/` (built by
`./parity/setup.sh`, needs Node and npm), the oracle-dependent tests, and code comments citing PARITY sections.
It said all of this was optional and to be left alone.

In practice the leftovers still cost something. Agents and the gate had to be told which steps fail without the
oracle; the docs described a contract nobody enforced; the comments pointed at a document that no longer binds
anything. The owner has now decided to remove parity validation entirely.

## Decision

Parity validation is removed from the repository:

- `docs/PARITY.md`, `docs/parity/` and the reviews that were about the parity contract.
- `cargo xtask parity` and `cargo xtask lint-scenarios`, the scenario corpus, the oracle setup under `parity/`,
  the probe harness, and the matching `xtask/gate.sh` steps.
- Tests that need the oracle, and generators that read the TypeScript source.
- Citations of PARITY sections and of the TypeScript original in code comments, which are rewritten to state the
  reason in r-orch's own terms.
- Documentation, instructions and README claims of a faithful port, or of compatibility with the npm package.

Behaviour is specified by r-orch's own docs, specs and tests. The README keeps one sentence of provenance
(r-orch began as a port of `oxgeneral/ORCH`); attribution is not a compatibility promise.

Kept from 0002: compatibility with `.orchestry` data written by existing r-orch releases is a normal
contract-change concern (a plan and, for contracts, an ADR). Reviewers reject accidental breakage of it.

## Consequences

- Supersedes the parts of 0002 that kept the tooling (`cargo xtask parity`, `fixtures/scenarios`, `parity/`
  optional) and left PARITY-citing comments alone. 0002's decision that matching the original is not required
  stands.
- Release and PR checks no longer carry parity, scenario-lint or probe steps; every remaining gate step must pass.
- Golden fixtures that are r-orch's own regression data stay (for example the shop catalogue).
- Older plans and the gate reviews in `docs/reviews/` are historical records and still mention the oracle and
  PARITY; they are not edited.
- Behaviour that once existed to match the original is now ordinary behaviour: changing it follows the usual
  contract-change rules (`--json`, on-disk and peer-protocol changes need a plan and an ADR).
- The cross-process check against an independent YAML 1.1 reader (js-yaml, through the oracle) is gone. YAML 1.1
  safety now rests on the hand-measured date table, the `fs_utils` guard unit tests and
  `crates/orch-infra/tests/store_round_trip.rs`.
