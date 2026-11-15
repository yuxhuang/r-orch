# 0014. Gate targets change only after their writers have ceased; a selected `orch-cli` couples `orch-infra`

- Status: proposed
- Date: 2026-11-14
- Supersedes: named clauses of [ADR 0013](0013-tiered-verification-gates.md) decisions 30 and 31, and
  qualifies its decisions 14, 15 and 21. The clauses are listed under "What this changes in ADR 0013".
  Everything else in ADR 0013 stands.

## Context

ADR 0013 is frozen at its revision 2 and its text is not rewritten. After it froze, the reviews of the gate's
test files and a feasibility check of signal handling showed that two of its decisions say things the project
cannot stand behind. The lead ruled on each point. This record carries those rulings into the decision text.
It adds no mechanism, chooses no runtime and relaxes no requirement of the
[spec](../specs/2026-11-14-tiered-checks.md). The [plan](../plans/2026-11-14-tiered-checks.md) (section 12,
fifth amendment) has the vectors and the work that follows.

Facts the decisions rest on. Each was read in the frozen documents, the frozen test and selector sources or a
retained review report. Nothing was run for this record.

- Decision 30 writes the warm record after a build "once its children have exited", and resets a target whose
  marker survived. A gate can wait for the children it started. Waiting for them tells it nothing about a
  descendant that left its process group, started its own session or was re-parented. Such a process can
  still write into the target after the record is published, after a reset, or after the cold target was
  removed. The next run then trusts artifacts that no record describes. That breaks the rule that nothing
  passes on a stale artifact (spec R3).
- This is not limited to interrupts and kills. A test or a build script can leave a writer behind after a
  normal finish and after a failed compilation as well.
- End of file on a channel, the reaping of the direct child and the absence of a process id seen earlier do
  not show that a descendant is gone: a descendant can close what it inherited and outlive its parent. The
  security review holds this as a blocking finding (review id FS2), also for a normal finish.
- The feasibility check of signal handling without a new dependency found no established way, on a macOS
  host where every agent runs under one account, to tell which processes belong to a run once a descendant
  is re-parented. It stopped before launching anything. An earlier independent review of a measurement
  supervisor on that host had observed such an escape: the leaf was missing from the list of owned
  processes and was alive after the supervisor exited. Both are retained findings and were not repeated.
  They describe the routes that were evaluated; they are not a proof about every possible mechanism.
- Decision 31 fires its coupling only when `orch-cli` itself changed (read in the frozen selector). A change
  in a crate that `orch-cli` depends on selects `orch-cli` through the normal closure and leaves `orch-infra`
  out. `fast` would then skip `concurrent_index`, whose subject is the daemon behind the `orch` binary it
  starts.
- Decision 31 also relies on cargo building that binary before the test starts, because both packages are
  named in one invocation. No document of cargo states that order as a guarantee, and no run has shown it.
  A warm target can hold an `orch` binary from an earlier tree.

## Decision

The numbers continue those of ADR 0013, so that a decision number names one decision across both records.
"A target" means a directory the gate owns and builds in: `target/gate` and `target/gate-warm`. "A use" is one
gate run's building and testing in a target. "A writer" is any process of a use that can write into its target.

### Target recovery

32. **A target is changed only after the writers of its previous use have ceased.** Four operations are
    covered: *publication* (writing the completed warm record and removing the marker), *removal* (deleting a
    target, including the cold target at the end of a run), *reset* (deleting a warm target to build from
    nothing) and *reuse* (building in a target that holds earlier artifacts). Each needs independently
    established cessation of every writer of the previous use. This holds however that use ended: a normal
    finish, a failed compilation, an interrupt the gate handled (INT, TERM or HUP), or the death of the
    holder. It holds for the warm and for the cold target. There is no exemption for a normal finish.
    Creating a target that does not exist is none of the four operations.

33. **What establishes cessation, and what does not.** Cessation is *independently established* by an
    observation that the gate's reviewed lifecycle contract names. That observation covers every process the
    use started, directly or indirectly, including one that left its process group, started a session or was
    re-parented. It does not rest on the cooperation of those processes. It counts only once the security
    and resilience reviewers have accepted it as part of the gate's boundary. None of the following
    establishes cessation:
    end of file on a channel; the reaping of the direct child; a recorded process id that is no longer
    present; the marker, present or absent; the end of a bounded wait; a signal that was handled; an
    operator's acknowledgement. Each is evidence for a diagnosis at most. None of them, and no number of them
    together, observes a re-parented descendant. The rule is about the gate's own uses. It does not ask the
    gate to certify unrelated activity of the account, which decision 22 samples and reports. Content that
    no gate use is known to have produced, such as a target planted by a plain cargo command, is not a
    previous use: decisions 14 and 15 delete or reset it as before, under the cooperative lease of decision
    21.

34. **Without established cessation the operation is refused.** Unknown ownership is treated as a live
    writer. The gate then does not publish, remove, reset or reuse; it leaves the target and the marker as
    they are, sends no signal to a process it cannot attribute, and says why, naming what it observed as a
    diagnostic. A wait for cessation is bounded. When it ends without proof, the outcome is this refusal: an
    expired wait is not permission, and neither is a process id that went missing during it. At the start of
    a run that needs a removal, a reset or a reuse, the refusal is a preflight refusal (`REFUSED`, exit 1, decisions 5
    and 18) and nothing builds. At the end of a run, the refusal leaves the target in place, unpublished.
    Recovery stays refused until cessation is established by decision 33's means; no later run, elapsed time
    or acknowledgement changes that.

35. **The warm record is published at established cessation, not at the end of the last command.** The
    marker and the source identities are written before the first building command, as decision 30 says.
    The record is written, and the marker removed, only when decision 32 is met for this use. That is so
    after a successful build, after a compile error and after a handled interrupt alike. The end time in the
    record is taken when cessation is established, not earlier: a writer that finished after an earlier end
    time could otherwise leave an artifact that looks newer than a source edit it never saw. A marker
    therefore survives every use whose cessation was not established, not only a gate that was killed. A
    run that finds a marker refuses (decision 34) unless the cessation of that earlier use is established;
    only then does it reset, with `reset: previous build did not finish`.

36. **Deleting the cold target and resetting the warm target are removals under decision 32.** Decision 14's
    deletion of `target/gate` before and after a run, its cleanup on failure or interruption, and decision
    15's resets keep their conditions and their reasons. Each is carried out only when decision 32 is met.
    A cold target that is left in place because cessation was not established is not a defect of the run
    that left it. A cold target removed under a live writer would be: the writer can recreate it, and the
    next cold run would start on a tree it believes is fresh.

37. **The lease holder is the process the caller started.** It may have replaced itself by `exec`. It is the
    only owner of the lease's file descriptor. No helper keeps that descriptor after the holder is killed,
    and no descendant inherits it. A helper that outlives the holder does not change a target, because the
    next run may already hold the lease; a signal from such a helper needs authority from a child it started
    and has not yet reaped. Three contracts stay distinct, and none stands in for another: the kernel frees
    the lease at once when the actual holder is killed with live descendants; a handled INT, TERM or HUP
    ends in a bounded teardown; a measurement supervisor may hold a lease of its own through its teardown.

38. **No number is chosen for the grace period of a handled interrupt.** The teardown is bounded; the bound
    has no value yet. The deadlines in the gate's test files are budgets of those tests. No product limit is
    taken from them, and none is stated as a supported property of the gate. The owner of the gate's
    implementation and of the release supplies a value and its justification once a workable route for
    decision 33 exists. Independent reviewers assess it. The test budgets are then derived from it, not the
    reverse.

### Selection

39. **A selection that contains `orch-cli` and not `orch-infra` also selects `orch-infra`.** The check runs
    after the normal reverse-dependency closure of decision 19, on its result, so it covers a change in
    `orch-cli` and a change in any crate that `orch-cli` depends on. The added package gets its own reason
    of class `coupled`. Packages that depend on the coupled package are not added. A change in `orch-infra`
    itself selects it and its dependents through the normal closure, with no `coupled` reason, as before.
    A selection of everything needs no coupling. The general rule for a row of this kind: when the selection
    contains the package whose binary is started by path and not the package whose test starts it, the
    second is added, alone.

40. **A test that starts a binary by path runs only after that binary was built from the same tree, in an
    order the gate sets.** For every run that includes such a test, the gate completes a build that brings
    the binary up to date in the tier's target, from the tree under test, and only then starts a test
    process. If that build fails, the test does not start. The order comes from the gate's own sequence of
    commands. It is not inferred from how cargo schedules units inside one invocation. Which commands
    achieve this is the implementation's choice. It is accepted on evidence: a probe on a two-package
    workspace in which the binary prints a marker from its source, run on an empty target and again after
    the marker changes, on macOS and on native Linux; and the real `concurrent_index` test run by itself
    against the integrated selection. Both are due and have not run.

## What this changes in ADR 0013

| ADR 0013 | Clause | Effect |
|---|---|---|
| Decision 30 | "When the last building command ends, with success, with a compile error or after an interrupt that the gate handled (once its children have exited), it writes the record with those identities and the end time, and removes the marker." | Superseded by decision 35 as to *when*: at established cessation, with the end time taken then. What the record holds, and that it is also written after a compile error or a handled interrupt, stand |
| Decision 30 | "The marker therefore survives only a gate that was killed." | Superseded by decision 35 |
| Decision 30 | "A run that finds it resets: `reset: previous build did not finish`." | Superseded by decisions 34 and 35: refused unless cessation is established, then reset with the same reason |
| Decision 30 | The marker before the first build; the meaning of "content changed without a newer modification time"; the limits on hosted runners | Stand |
| Decision 31 | "a change in one package also selects another package whose tests run the first one's binary by path" and "a change in `orch-cli` also selects `orch-infra`" | Superseded by decision 39 as to the trigger: the selection after the normal closure, not the changed path |
| Decision 31 | "`fast` runs all selected packages in one cargo invocation, so that binary is built from the same tree before the test starts." | Superseded by decision 40 |
| Decision 31 | The table is kept by hand; a coupling missing from it is caught by `pr`, not by `fast` | Stand |
| Decision 14 | Deletion of `target/gate` before and after the run; cleanup on failure or interruption | Qualified by decisions 32 and 36. No word is replaced |
| Decision 15 | The resets of `target/gate-warm` and the `reused` result | Qualified by decisions 32 and 36. No word is replaced |
| Decision 21 | The holder of the lease | Qualified by decision 37. No word is replaced |

Every other decision, consequence and alternative of ADR 0013 is unchanged.

Until this record is accepted, the same points bind as the lead's rulings recorded in the plan (section 12,
holds K-1 to K-5 and the broader coupling). A rejection of this record does not release them.

## What this record does not do

It removes permissions that contradicted the rulings. It does not supply the observation that decision 33
requires. No reviewed way exists today to establish, on a macOS host where all agents share one account,
which processes belong to a run after a descendant is re-parented. Until one is established and accepted by
the security and resilience reviewers, the gate cannot publish, remove, reset or reuse a target there, and
the folded gate and the measured baseline stay blocked. This record does not turn that block into a passing
state: a gate that always refuses does not meet decisions 14, 15 and 30, the vectors that expect `reused`,
a reset or a removed cold target, or the timing requirement of the warm tiers. Whether native Linux offers
such an observation has not been evaluated.

Not decided here, and the decisions above do not depend on it:

- which mechanism establishes cessation, and on which hosts;
- how a later run knows durably that a gate use began in a target and did not reach established cessation.
  The marker of decision 30 does this for the warm target. The cold target has no equivalent in ADR 0013
  and needs one, because a killed `full` run must be recognizable to the next run;
- the exact wording of the refusal;
- what verdict a run reports when its steps passed and it could not establish cessation at its end, and
  whether such a run can qualify as evidence (decision 17). Proposed, for the lead and the gate's owner: the
  step verdicts stand, the result says that the target was kept and why, and the run does not qualify;
- a fresh target path instead of a reset, and a dedicated account for gate runs. Neither was evaluated;
  neither is adopted or ruled out.

A limit that stays: the gate and its operator share one account. A marker deleted by hand looks to the next
run like a target that was never in use. No document offers that as a recovery, and the gate cannot detect
it.

## Consequences

- The warm record now means "this tree was built here and its writers are known to have stopped". A reuse
  rests on that, not on the gate having outlived its direct children.
- A host without an accepted observation produces no reusable warm target and keeps its cold target. The
  price is paid in the open, as a refusal with a reason, not as a silent stale artifact.
- The frozen test files expect behaviour of decision 30 that is now superseded. The plan lists the edits;
  none is made or authorized by this record, and no expected protection is dropped.
- The selector frozen for PR 2 implements the narrower trigger of decision 31. Its change, the added table
  case and the build-order evidence are PR 2 work.
- `fast` selects `orch-infra` for most code changes, because `orch-cli` depends on every other crate. The
  cost in time is not measured.
- A run with a path-coupled test may build the binary in a separate step first. The cost is not measured.

## Alternatives rejected

- **A23. Disclose a surviving process and reset the same target.** A disclosure stops no writer, and a writer
  keeps writing into the reset tree.
- **A24. Apply the rule to interrupted and killed runs only.** A test or a build script leaves writers behind
  after a normal finish too, and the exemption would contradict the open security finding on exactly that
  case.
- **A25. Let an operator's acknowledgement authorize recovery.** A statement by a person observes no
  process, and the gate has nothing to check it against.
- **A26. Treat the end of a bounded wait, or a recorded process id that is gone, as permission.** A process
  id seen once is not an identity, and its absence says nothing about a re-parented descendant.
- **A27. Take the grace period from the deadlines of the test files.** A test budget would then define the
  product limit it is meant to check.
- **A28. Keep the narrower trigger of decision 31.** `fast` would skip `concurrent_index` for a change in the
  daemon that the test exercises.
- **A29. Also select the packages that depend on the coupled package.** They do not start the binary; `pr`
  runs them in any case.
- **A30. Rely on one cargo invocation to order the build.** The order is undocumented and unmeasured, and a
  warm target can already hold an older binary.
