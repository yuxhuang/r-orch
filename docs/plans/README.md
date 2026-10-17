# Plans

One file per piece of non-trivial work, written before the code. Adopted in
[ADR 0001](../decisions/0001-plans-and-decision-records.md).

## When a plan is required

The change alters an on-disk, CLI, `--json` or peer-protocol contract; adds a subsystem or a load-bearing
dependency; will take more than about a day; or the lead asks for one. Crossing crates alone does not require a
plan. Other fixes need none.

## Naming

`YYYY-MM-DD-<slug>.md`, dated when the plan is first written, slug in lower-case kebab-case.
Start from [TEMPLATE.md](TEMPLATE.md).

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
- Keep finished plans; they are the record of what was intended.
- Decisions with lasting consequences also get an [ADR](../decisions/README.md); link it from the plan.
