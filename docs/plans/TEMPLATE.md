# <Title>

status: proposed
owner: <role or name>
date: YYYY-MM-DD
spec: [../specs/YYYY-MM-DD-<slug>.md](../specs/YYYY-MM-DD-<slug>.md) (or "none: <reason>")

## Goal
One or two sentences: what is true when this is done.

## Non-goals
What this deliberately does not do.

## Context
Why now; relevant code, docs, issues, ADRs (links).

## Design
The approach, the crates and files touched, and the alternatives rejected in one line each. About a page; lasting
choices go in an ADR.

## Tasks

| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| T1 | Walking skeleton: <thinnest end-to-end slice> | R1 | `crates/<crate>/...` | — | `cargo test -p <crate> --test <file>` | 1 |
| T2 | ... | R2, R3 | | T1 | | 2 |
| T3 | Docs updated (see below) | | | | | |

## Status

| Item | State (implemented / verified on <platform> / pending) |
|---|---|
| T1 | |

## Test plan
What proves it works: the spec's acceptance checks, other unit/integration tests, the CI set; the cold
`./xtask/gate.sh` if layering, process handling or security is touched. Say which platform runs each. For
dashboard changes, name the tmux end-to-end tests (AGENTS.md, "TUI end-to-end tests").

## Rollback / compatibility
What happens to existing `.orchestry` data and to peers on other versions; how to back out.

## Docs to update
README / RUNBOOK / ARCHITECTURE / CHANGELOG / ADR, as applicable.

## Open questions
- ...
