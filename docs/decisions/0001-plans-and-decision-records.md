# 0001. Plans and decision records

- Status: accepted
- Date: 2026-10-16

## Context

The project is developed by an orchestrator and a team of AI agents in different roles. Until now, intent lived
in chat, PR descriptions and the port-era documents (`docs/PARITY.md`, `docs/reviews/`). Those are fine for a
fixed spec, but they leave two gaps: where in-flight work is agreed before code is written, and where the
reasons behind a choice are kept after it is made. Agents start each session without memory, so anything not in
the repo is lost.

## Decision

Adopt two lightweight directories, described in `AGENTS.md`:

- `docs/plans/`: one file per piece of non-trivial work (contract change, new subsystem or load-bearing
  dependency, more than about a day, or the lead asks; crossing crates alone does not qualify), named `YYYY-MM-DD-<slug>.md`, with a `status:` line moving
  `proposed -> active -> done | abandoned`. The plan lands before or with the implementation.
- `docs/decisions/`: numbered ADR-lite records (`NNNN-<slug>.md`) with Status, Context, Decision and
  Consequences, for irreversible or cross-cutting choices.

Trivial fixes need neither. `docs/reviews/` stays as a historical archive and receives no new files.

## Consequences

- Multi-crate work has a short written agreement to review before code exists.
- Future agents and contributors can find why, not just what.
- Small overhead: one file per qualifying change. If plans go stale in practice, the rule to tighten is "update
  or abandon", not to add process.
- `AGENTS.md` and the PR template must keep pointing at these directories.
