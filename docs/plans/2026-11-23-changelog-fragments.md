# Changelog fragments and release assembly

status: proposed
owner: lead; one documentation writer for this wave
date: 2026-11-23
spec: [../specs/2026-11-23-changelog-fragments.md](../specs/2026-11-23-changelog-fragments.md)

## Goal

Release notes are written as one file per entry and assembled by one command at release, and the PR rules of the
spec's Source item 1 are in AGENTS.md. R1 to R16 of the spec hold when the tasks below are done.

## Non-goals

Converting existing notes; changing the release, tag or Homebrew steps; adding a dependency. The historical
[SQLite plan](2026-11-20-sqlite-storage.md) is not edited: its documentation deliverable (R) is met by assembling
the 1.1 entry.

## Context

[ADR 0021](../decisions/0021-changelog-fragments.md). `xtask` already holds `crate-graph`, `fixture-gen`, `linux`
and `gate`; the new subcommand follows `xtask/src/fixture_gen.rs`.

## Design

One PR, based on `main`, from two commits: the documentation commit (AGENTS.md, this spec, plan and ADR) and the implementation commit (below). Writers and builders do not share a path.
Details are in the spec; the implementation is `xtask/src/changelog.rs` plus `xtask/tests/changelog.rs`, a
`Changelog` variant in `xtask/src/main.rs`, and the files the documentation commit cannot own (see Tasks).

## Tasks

| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| T1 | Documentation commit: AGENTS.md, spec, plan, ADR | R15, R16 | `AGENTS.md`, `docs/specs/*changelog-fragments*`, `docs/plans/*changelog-fragments*`, `docs/decisions/*changelog-fragments*` | — | `./xtask/gate.sh fast --base origin/main` (docs-only); links and commands read against the code | 1 |
| T2 | `check` and `assemble` with the golden example, refusals and failure injection | R2 to R12 | `xtask/src/changelog.rs`, `xtask/src/main.rs`, `xtask/tests/changelog.rs`, `xtask/tests/fixtures/changelog/` | T1 | `cargo test -p xtask --test changelog` | 1 |
| T3 | Edits outside T1's paths that the new rules require: the `CHANGELOG.md` header sentence "Add entries under `[Unreleased]` in the PR that makes the change"; `.github/PULL_REQUEST_TEMPLATE.md` ("CHANGELOG (Unreleased)" becomes "Changelog fragment"); and `docs/changelog/unreleased/README.md` | R1, R13 | `CHANGELOG.md` (header only), `.github/PULL_REQUEST_TEMPLATE.md`, `docs/changelog/unreleased/README.md` | T1 | `cargo xtask gate select --base origin/main --format json` shows `docs-only` for a fragment-only diff | 1 |
| T4 | CI runs `check` (owner decision) | R14 | `.github/workflows/ci.yml` | T2 | workflow diff | follow-up |
| T5 | Release 1.1 assembles with the real command and records the output in the spec's Verification | R6 to R11 | release PR | T2 | recorded run | release PR |

## Risks

- A PR opened before the cutover that still edits `[Unreleased]` conflicts with the cutover PR; it is rebased or
  switches to a fragment.
- Reviewers must check file handling (symlinks, size, names) in T2: spec Constraints.

## Done

All `must` rows of the spec pass; the Verification table is filled in; status `done` is set by the PR that
completes T5.
