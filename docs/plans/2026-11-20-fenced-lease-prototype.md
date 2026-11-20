# Fenced lease prototype implementation plan

status: active
owner: senior engineer
date: 2026-11-20
spec: [experiment spec](../specs/2026-11-20-fenced-lease-prototype.md)

## Goal and design

Produce executable evidence for entry step E, outside all product crates. One standalone
Cargo workspace under `prototypes/fenced-lease/` uses the library recommendation
rusqlite 0.41.1, bundled SQLite and its online backup API. Dependencies are declared in
that workspace's dependency table, never the product workspace. No application imports it.

The schema and database operations live in `src/lib.rs` and `src/schema.sql`;
`src/holder.rs` models holder deadlines; `src/tests.rs` exercises concurrent connections,
transaction rollback, and deterministic virtual elapsed/wall clocks. No new clock library
is needed because no real clock is read. `docs/fenced-lease-prototype.md` is the report.
[ADR 0018](../decisions/0018-fenced-lease-prototype.md) records experimental choices.

Only current attached leases accept writes. Restart creates a new session and marks all
leases recovering, reserving both task and agent until a monotonic 120 s grace deadline.
Reattachment proves the existing run token and returns the new session token; it does not
create a run or an attempt. Restore instead invalidates all old leases and selects a new
lineage; counters are repaired to at least restored run/lease maxima before new claims.

## Tasks

| Id | Task | Covers | Scope | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| T1 | Write experiment spec, plan and proposed ADR before code | R1–R7 | named docs | — | source/acceptance trace | this PR |
| T2 | Test constraints and claim transaction, including every rollback point | R1,R2 | prototypes/fenced-lease | T1 | standalone cargo test | this PR |
| T3 | Model holder, restart and restore; test counterexamples and corrections | R3–R6 | prototypes/fenced-lease | T2 | standalone cargo test | this PR |
| T4 | Record results, dependency check, limitations and spec corrections | R1–R7 | docs/fenced-lease-prototype.md | T3 | fmt, clippy, deny, fast after commit, pr before PR | this PR |

## Execution and review focus

- [x] Author design documents before implementation; build only the explicitly requested spike.
- [x] Write behavioral tests first; verify missing behavior, implement, then rerun.
- [x] Cover stale result/renewal, same-agent different-task race, rollback at every write,
  exact expiry/grace boundaries, delayed acknowledgement and counter overflow.
- [x] Compare snapshot counterexample to corrected lineage fencing; do not label numeric
  epochs globally unique when only the compound identity is unique.
- [ ] Commit, run fast, run pr before opening a PR, report every failure and limitation.

## Review correction round

F1 adds the proven-terminated-holder restore precondition, not product enforcement.
F2/F3 add two tests and reuse the session label across restore; M1 and M10 must fail
under mutation. F4 exposes the 90 s whole-system-sleep consequence for owner choice.
Final-head standalone checks and repository fast/pr are required again; delivery
stays incomplete until fixture prerequisites land and all mandated checks pass.

## Rollback and documentation

Delete the experiment directory and documents to remove it. Existing data, peers, CLI,
product manifests and runtime behavior stay as on main. README/RUNBOOK/ARCHITECTURE/
CHANGELOG need no changes because no product behavior changes. Future product code is
blocked by the goal's owner approval gate regardless of this experiment's outcome.
