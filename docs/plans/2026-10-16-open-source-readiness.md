# Open-source readiness

status: proposed
owner: owner
date: 2026-10-16

## Goal
The repo has what an open-source project needs (licence, security contact, enforced checks, working automation),
so that flipping visibility is a single owner decision with no known gaps. This plan does not flip it.

## Non-goals
Making the repo public. Adding features. Changing the TypeScript-original relationship beyond wording.

## Context
The repo is private and being prepared for open source. Gaps found while
setting up agent-facing process ([ADR 0001](../decisions/0001-plans-and-decision-records.md)). The original
`oxgeneral/ORCH` is MIT.

## Design
Independent checklist items; each is its own small PR or a repository-settings change by the owner.

## Steps
- [ ] Add a `LICENSE` file. `Cargo.toml` declares MIT but no file exists. The original's copyright notice must be
      retained; the owner decides the copyright line for r-orch's own work.
- [x] Fix `Cargo.toml` `repository`, which points at `oxgeneral/r-orch` (done in the agents/SDLC PR).
- [ ] Add `SECURITY.md` with a private disclosure channel.
- [x] Make `ci / check` and `ci / test` required status checks on `master`, after their first green run (owner,
      repository settings).
- [x] Fix the ruleset's include pattern. It is now `refs/heads/release/**`. That pattern also makes
      `release/vX.Y.Z` branches protected, so release PRs come from `chore/release-vX.Y.Z` instead (AGENTS.md).
- [ ] Verify `require_extra_approval_for_unattributed_changes` does not deadlock a solo owner.
- [ ] Decide on dependabot (cargo and github-actions, monthly) versus the weekly scheduled cargo-deny run.
      (The weekly schedule is added to `ci.yml` in the agents/SDLC PR; mark done once merged.)
- [ ] Optional, deferred: a `release.yml` workflow on `v*` tags.
- [ ] Check the README "Relationship to the TypeScript original" wording.

## Test plan
Each item is verifiable by inspection: file present, setting visible in the ruleset, workflow run green.

## Rollback / compatibility
Repository settings can be reverted by the owner; file additions revert with the PR. No effect on `.orchestry`
data or peers.

## Docs to update
CHANGELOG `Unreleased` (done for this plan); README Documentation table if a new top-level file is added.

## Open questions
- Copyright holder line for `LICENSE`.
- Which private disclosure channel `SECURITY.md` names.
