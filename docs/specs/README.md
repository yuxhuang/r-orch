# Specs

A spec says **what** a piece of work must do and **how we will know it does it**. A [plan](../plans/README.md)
says how we will build it. Adopted in [ADR 0004](../decisions/0004-spec-driven-development.md).

## When a spec is required

Write one when a plan is required (see [plans](../plans/README.md#when-a-plan-is-required)) **and** the work has
behaviour a user or operator will notice. An owner request that meets no plan trigger needs no spec. Pure
internal work (a refactor, a dependency swap, a release) needs a plan at most, with the requirements in its Goal
section. Fixes need neither.

## Naming

`YYYY-MM-DD-<slug>.md`, dated when first written. The plan that implements it uses the same slug, so the pair is
`docs/specs/2026-11-02-foo.md` and `docs/plans/2026-11-02-foo.md`. Start from [TEMPLATE.md](TEMPLATE.md).

## What goes in a spec

- **Source.** The owner's request, quoted verbatim, with each item numbered. Do not paraphrase it away; it is
  what the requirements are checked against.
- **Requirements.** One row per requirement, with a stable id `R1`, `R2`, ... and never renumbered. Write each as
  one testable sentence in the form *When \<trigger\>, \<component\> shall \<response\>*, or *\<component\>
  shall \<response\>* when it always holds. Give each a source (an owner item number, or the review finding that
  added it), a priority (`must` / `should` / `won't`), and an acceptance check. Every owner item maps to at least
  one requirement or to an explicit `won't` row with a reason.
- **Acceptance check.** A test path and name (`crates/orch-tui/tests/text_viewer.rs::<test_name>`), or an exact
  command with its expected result, plus the platform it runs on. "Reviewed" is not an acceptance check.
- **Non-goals** and **constraints.** For anything that listens on a socket, crosses a process boundary or
  handles untrusted text, fill in the security checklist in the template before building, not at the gate.
- **Verification environment.** What runs on a developer's machine, what only runs on Linux CI, and what needs
  the owner's permission or hardware (a real terminal profile, a second host, a native clipboard). Ask for these
  in the first approval request. Do not discover them at the end.
- **Open questions** for the owner.

A spec contains no design. Choices of crate, module, type or layout go in the plan or an ADR. Keep it short:
most specs fit in about a page. If a spec passes about 150 lines, it is probably carrying design.

## Lifecycle

The `status:` line at the top is the single source of truth.

| Status | Meaning |
|---|---|
| `draft` | Written, not yet approved. |
| `approved` | The owner agreed to the requirements. The lead may approve them only where the owner's request says so; quote that line in Source. Build may start. |
| `done` | Every `must` row passes, or is recorded as `waived by owner`. An `env-limited` `must` row blocks `done` until it passes where it can run. Set in the PR that completes the work. |
| `superseded` | Replaced; link the replacement. |

Rules:
- The spec lands before, or in the same PR as, the plan.
- Requirements may be **added** after approval. A review finding that is a real requirement (a size limit, a
  panic on resize) becomes a new `R` row whose source is the finding. It does not become a footnote in the
  plan. Changing or dropping an approved `must` row needs the owner.
- Keep finished specs; they are the record of what was promised.

## Traceability

The acceptance column is the trace: requirement → test → result. Test names describe behaviour; do not put spec
ids in code comments or test names, so a superseded spec does not leave markers behind in the code.

The PR that completes the work fills the spec's **Verification** table: for each requirement, the result
(`pass`, `env-limited`, `waived by owner`), the commit the check ran at, and the platform. The rules:
- A failure that was already on the base commit is still a failure, not a pass.
- `env-limited` means the check could not run where the work was done. Name where it must run instead.
- List any defect found outside the requirement rows, and what happened to it.
