# Decision records

Short records of choices that are costly to reverse or that others will later ask about. Adopted in
[0001](0001-plans-and-decision-records.md).

## When to write one

- An irreversible or cross-cutting choice (storage layout, protocol, crate boundaries).
- Adopting or replacing a load-bearing dependency (see ARCHITECTURE §4.8).
- A change to an on-disk, CLI, `--json` or peer-protocol contract.
- Any change to `docs/TRAITS-FROZEN.md`.
- Dropping a requirement the project previously held.

Not needed: bug fixes, refactors inside one crate, routine dependency bumps. If in doubt, write five lines
rather than none.

## Format

File `NNNN-<slug>.md`. Keep it to about one page.

```markdown
# NNNN. Title

- Status: proposed | accepted | superseded by NNNN | rejected
- Date: YYYY-MM-DD

## Context
What forced the decision; the facts and constraints, with links.

## Decision
What we will do, in the active voice.

## Consequences
What gets easier, what gets harder, what must change as a result.
```

## Numbering and lifecycle

- Four digits, sequential, taken from the highest number on `master` plus one. If two PRs collide, the later one
  renumbers before merge.
- Never reuse or renumber a merged ADR.
- Do not rewrite an accepted ADR's decision. Supersede it with a new ADR and change the old status line to
  `superseded by NNNN`. Typo and link fixes are fine.
- Land the ADR in the same PR as the change it justifies, or before it.

## Index

| # | Title | Status |
|---|---|---|
| [0001](0001-plans-and-decision-records.md) | Plans and decision records | accepted |
| [0002](0002-parity-is-no-longer-a-goal.md) | Parity with the TypeScript original is no longer a goal | accepted |

`docs/reviews/` holds historical gate reviews from the port; new material does not go there.
