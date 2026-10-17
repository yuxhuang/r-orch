# Plans

One file per piece of non-trivial work, written before the code. A plan says **how** the work is built and in
what order; its [spec](../specs/README.md), when there is one, says **what** must be true. Adopted in
[ADR 0001](../decisions/0001-plans-and-decision-records.md), revised by
[ADR 0004](../decisions/0004-spec-driven-development.md).

## When a plan is required

The change alters an on-disk, CLI, `--json` or peer-protocol contract; adds a subsystem or a load-bearing
dependency; will take more than about a day; or the lead asks for one. Crossing crates alone does not require a
plan. Other fixes need none. If the work also has user-visible requirements, write the spec first.

## Naming

`YYYY-MM-DD-<slug>.md`, dated when the plan is first written, slug in lower-case kebab-case and the same as its
spec's. Start from [TEMPLATE.md](TEMPLATE.md).

## What goes in a plan

- **Design**, about a page: the approach, the crates and files touched, and the rejected alternatives in one
  line each. A choice with lasting consequences goes in an ADR, linked from here. Detail that a test can state
  (cell geometry, exact limits, byte budgets) goes in the test, not in prose.
- **Tasks**: one row per unit a single agent can finish and verify, each with the requirement ids it covers,
  the paths in scope, its dependencies, the exact verify command and the PR it lands in. This is the PR split,
  decided up front. A task that cannot name a verify command is not ready to hand out.
- **Status table**: what is implemented, what is verified and on which platform, what is pending. Edit it
  whenever it changes.
- Evidence (logs, measurements, review transcripts) goes in PR descriptions and CI, not in the plan. Link it.

Most plans fit in about 300 lines. If one grows past that, move the design into an ADR or the code. Do not
split it into versions.

## Design review

The loop is bounded by what each round finds, not by how many rounds there are:
- A design gets one independent challenge, then one rebuttal from its author. Another round is justified only
  if the last one found something backed by executable evidence (a failing test, a compile probe, a reproduced
  behaviour) or a blocking contract or safety problem.
- If an objection can be settled by a test or a probe, write the test or probe. Do not write another revision.
- Before a second design revision, build a walking skeleton: the thinnest end-to-end slice, with the first
  acceptance test failing for the right reason.
- When a round ends the loop, its open non-blocking points become follow-ups or new spec requirements. Open
  blocking points go to the owner.
- The reviewer who challenged a design does not build the slice they challenged.

## Lifecycle

The line `status:` at the top of the file is the single source of truth.

| Status | Meaning |
|---|---|
| `proposed` | Written, not yet agreed. Open questions may remain. |
| `active` | Agreed; implementation in progress. |
| `done` | Shipped; checklist complete. Set in the PR that finishes it. |
| `abandoned` | Dropped; add a one-line reason. |

Rules:
- The plan lands before, or in the same PR as, the first implementation PR.
- If the work diverges from the plan, edit the plan in the PR that diverges.
- Set `done` (and the spec's `done`) in the PR that completes the last task; reviewers check it.
- Keep finished plans; they are the record of what was intended.
- Decisions with lasting consequences also get an [ADR](../decisions/README.md); link it from the plan.
