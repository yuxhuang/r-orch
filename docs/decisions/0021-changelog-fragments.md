# 0021. Changelog fragments, assembled at release

- Status: proposed
- Date: 2026-11-23

## Context

Release notes are lines under `## [Unreleased]` in `CHANGELOG.md`, added by each PR. Every PR therefore edits the
same lines, and two PRs that do not otherwise touch each other conflict there. Open PRs 207 and 214 both edit
them. PRs are qualified together in trains, so a conflict in a shared text file defers a member that is otherwise
ready. The repository has no dependency for this and the owner wants none added. Details are in the
[spec](../specs/2026-11-23-changelog-fragments.md).

## Decision

- Each PR adds one file per entry under `docs/changelog/unreleased/`, named `<slug>.<kind>.md`, holding one line.
  No PR edits `CHANGELOG.md`.
- The release PR runs `cargo xtask changelog assemble --version X.Y.Z --date YYYY-MM-DD`. It writes the release
  section and the compare links, keeps the existing Unreleased text unchanged, and deletes the fragments it used.
- The directory is under `docs/` because `cargo xtask gate select` classifies `docs/**` as documentation-only and
  an unlisted top-level path as `all` (`xtask/src/gate_selection/classify.rs`).
- Fragments carry no PR number: it does not exist when the first commit is made, and a second commit to add it
  would change the head and restart review and CI.
- The assembler is a hand-written `xtask` subcommand with no new dependency.

## Consequences

- Release notes no longer conflict between PRs; a repeated fragment name is the only add/add conflict.
- A malformed fragment is found by `check` or by `assemble`, not by the reader of `CHANGELOG.md`; CI does not run
  `check` until the owner merges a workflow change (spec R14).
- The first release after the cutover carries the existing hand-written Unreleased text unchanged, ahead of
  the generated sections, and may show two `### Added` headings until a person edits them.
- Rejected: (a) `towncrier` or another tool, because it adds a dependency and a Python toolchain to a Rust repo;
  (b) numbering fragments by PR number, because of the extra commit; (c) deriving numbers from `git log` or
  `gh`, because it adds a subprocess and network or full-history dependence to a file-handling tool; (d) a root
  `changelog.d/`, because it would select the full suite for every PR.
