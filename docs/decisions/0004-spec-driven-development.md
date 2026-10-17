# 0004. Spec-driven development with bounded design loops

- Status: accepted
- Date: 2026-10-17
- Supersedes: [0001](0001-plans-and-decision-records.md)

## Context

ADR 0001 introduced `docs/plans/` and `docs/decisions/` and said: "If plans go stale in practice, the rule to
tighten is 'update or abandon', not to add process." This ADR adds process, so it supersedes 0001. The evidence
for doing so comes from three days of orchestrator-driven goals on this repo (2026-10-15 to 10-17, about 1,650
agent runs):

- One feature goal ran design review, repair and re-review for about twelve hours. It produced about a thousand
  coordination files (design, review, handoff and log artifacts) and almost no code, and was discarded. Its redo
  reached a gated build in about an hour after one challenge round, though gate and fix loops then ran for about
  twelve more hours.
- Another goal went through six design versions. Its plan grew to about 700 lines and mixed requirements,
  design, status and evidence. The owner's nine numbered requests were not traced to requirement ids or tests
  in the plan. A real panic in `clamp_split_offset` was found in design round three by compiling the pinned
  source, not by more prose.
- "Done" was asserted without evidence: tasks were marked complete with no commit, runs that hit the turn cap
  (98) were still recorded as succeeded, and failures that already existed on the base commit were labelled
  baseline rather than fixed.
- Roughly half of one goal's elapsed time (about 8 of 17 hours) went into building an external harness around a
  repo script that was not portable to macOS. A one-line fix was ruled out of scope.

The common failure: nothing recorded what "done" meant in a form the owner could approve and a test could
check, and design rounds had no stopping rule tied to evidence.

## Decision

- **Specs.** Add `docs/specs/` (see its README). A spec records the owner's request verbatim, numbered
  requirements in the form "when \<trigger\>, \<component\> shall \<response\>", an acceptance check for each,
  constraints (including a security checklist for network, process-boundary and untrusted-text work) and the
  verification environment. It is required when a plan is required and the work has user-visible behaviour.
  The owner approves it before the build starts (the lead only where the owner's request says so). Reviews may
  add requirements.
- **Plans** keep ADR 0001's triggers and lifecycle. They become execution plans: about a page of design, a task
  table that fixes the PR split and gives every task a verify command, and a status table. Evidence lives in
  PRs and CI.
- **Design loops are bounded by yield.** After one challenge round and one rebuttal, another round happens
  only if the last one found something backed by executable evidence, or a blocking problem that names the
  requirement or contract it breaks. The lead decides; from the third round on it records why in the plan or
  escalates to the owner. An objection that a test can settle is settled by a test. A walking skeleton comes
  before a second revision.
- **Traceability lives in the spec**, not in code markers. The completing PR fills in the spec's Verification
  table with each result, the commit it was checked at, and the platform.
- **Decisions** are unchanged from 0001: `docs/decisions/`, ADR-lite, numbered, superseded rather than
  rewritten. `docs/reviews/` stays a historical archive.

## Consequences

- One more short file for each goal-sized piece of work. Fixes and internal work are unaffected.
- The owner approves requirements, not designs; designs can change without re-approval as long as the
  requirements still pass.
- Size limits and lifecycle rules are discipline rules until a check enforces them. Code markers that point at
  a spec were rejected because they rot the way the old `PARITY` references did.
- Review: after the first two goals that use specs, check whether any spec requirement caught a defect or a
  missed request that the plan alone would not have caught. If none did, retire `docs/specs/` with a new ADR.
