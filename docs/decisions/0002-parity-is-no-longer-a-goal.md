# 0002. Parity with the TypeScript original is no longer a goal

- Status: superseded by [0009](0009-remove-parity-tooling.md)
- Date: 2026-10-16

## Context

r-orch began as a Rust port of `oxgeneral/ORCH`, with `docs/PARITY.md` as the contract and a differential
harness (`cargo xtask parity`) comparing both binaries. The owner decided on 2026-10-15 to drop parity. The port reached feature
completeness and has since shipped features the original does not have (remote agents and peers in 0.5.x,
`orch web`, TUI mouse support). Caveat: the original's FAQ mentions adding tasks "from another terminal or
machine", but the peer protocol, `orch web` and mouse support are still r-orch-only. Holding every behaviour to the original now only slows new work.

## Decision

Matching the TypeScript original is no longer a requirement. New features and fixes may diverge freely, including
CLI output, `--json` shapes and on-disk formats, subject to the normal rules for contract changes (a plan and, for
contracts, an ADR).

## Consequences

- `docs/PARITY.md` and `docs/parity/` are historical. They carry a banner and are not updated. Do not add
  PARITY §14 entries.
- Existing tests stay in the suite and must stay green. If a test fails only because it encodes original
  behaviour that we have chosen to change, flag it in the PR as parity-only and update or delete the test with
  an explanation; do not contort the code to satisfy it.
- `cargo xtask parity` and `fixtures/scenarios` remain available but are optional. `./parity/setup.sh` and
  `parity/orch-ts/` are only needed to run them; `parity/orch-ts/` stays uncommitted.
- Comments in code that cite PARITY sections are left alone. They explain why code looks the way it does, and
  mass edits would add noise.
- Reviewers no longer reject a change for diverging from the original or for lacking a parity entry. They still
  reject accidental breakage of existing users' `.orchestry` data: compatibility with files already written by
  r-orch releases is a normal contract-change concern, not a parity one.
- Cross-compatibility with the npm package, as described in the README, is no longer promised for new
  behaviour.
