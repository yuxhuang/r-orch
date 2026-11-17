# 0014. Gate targets change only after their writers have ceased; a selected `orch-cli` couples `orch-infra`

- Status: accepted. Since 2026-11-15 a decision of the owner overrides the clauses of decisions 32–38 below
  that demand independently established cessation of every writer. That decision is recorded as decision
  41 of [ADR 0015](0015-cooperative-gate-target-recovery.md) and is binding. ADR 0015 itself is proposed:
  its decisions 42–47 would supersede the clauses of this record that it names, and they do not do so
  until it is accepted. When it is, this line changes to name the supersession. Decisions 37, 39 and 40
  are not affected. The text from the next line to the end of this record is the text accepted on
  2026-11-14. It is not rewritten.
- Date: 2026-11-14
- Revision: 2 (corrected after the security and resilience reviews of revision 1), accepted on 2026-11-14
  only together with the lead's lifecycle clarification of decision 33 (see "Acceptance" below). The text
  of revision 2 without that clarification was not accepted.
- Supersedes: named clauses of [ADR 0013](0013-tiered-verification-gates.md) decisions 30 and 31, and only
  those. It also qualifies decisions 14, 15, 17 and 21 without replacing a word of them. The clauses are
  listed under "What this changes in ADR 0013". Everything else in ADR 0013 stands.

Changes in revision 2. Decisions 32–40 keep their numbers. No mechanism is added and no number is chosen.

- Decision 33: whether an existing target has a previous use is decided by affirmative provenance. The
  exception of revision 1 for content "that no gate use is known to have produced" is withdrawn. Unknown
  history is refused, for the warm and for the cold target.
- Decision 34: a run whose cessation is unresolved does not qualify as evidence in any tier, and its result
  shows that the target was kept and why.
- Decisions 35 and 36: when the cessation of a use is established only after that use ended, the next run
  resets or removes the target and never reuses it. An existing cold target is refused at the start of a
  run until durable provenance exists.
- Decision 38: every bound of the teardown and of a wait for cessation is listed. None has a value.
- "What this record does not do": the durable provenance is named as a load-bearing prerequisite that is
  not implemented. The sentence that the decisions did not depend on it is withdrawn.
- "What this changes in ADR 0013": the effects are stated as proposed; a row for decision 17 is added.
- Alternatives A31 and A32 are added.

Acceptance. The lead accepted this record after the same security and resilience reviewers had re-checked
revision 2 at its exact commit. The acceptance is of revision 2 together with one binding clarification,
and not of revision 2 without it. The security re-check had found that the text on the warm trace required
the marker and the completed record to be present at once, which decision 35 rules out for a use that
closed. The clarification is quoted in decision 33. With it, and with nothing else:

- the header says `accepted`, and the supersession of the named clauses of ADR 0013 is in force;
- decision 33 and the prerequisite paragraph of "What this record does not do" state the provenance that
  each lifecycle state requires, in place of "present, readable and complete" for both files at once;
- the sentence on the measured baseline in "What this record does not do" follows the owner's later
  decision on that measurement.

The list of changes above describes revision 2 as it was reviewed, when it was still proposed. Decisions
32–40 keep their numbers and their rules. No mechanism is added, no provenance is designed and no number
is chosen. The acceptance is of the design. It clears no runtime hold: the observation of decision 33 and
the durable provenance are still absent ("What this record does not do").

## Context

ADR 0013 is accepted at its revision 2 and its text is not rewritten. After it froze, the reviews of the gate's
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
    gate to certify unrelated activity of the account, which decision 22 samples and reports.

    Whether an existing target has a previous use is decided by affirmative provenance, never by the absence
    of a known use. There are three cases. Only the first two permit an operation.

    - *Never used by a gate.* It is independently established that no gate use ever began in this target,
      and its content is a closed, controlled plant: for example the target that a plain cargo command built
      and has finished building (the plan's vector TV13). Decisions 14 and 15 then delete or reset it as
      before, under the cooperative lease of decision 21.
    - *A previous use that is closed.* A gate use began in this target, and the cessation of its writers was
      independently established as this decision says. Decision 32 is then met. This is a separate case
      from the first and needs its own proof. No token proves cessation: not a marker, a use token or a
      record, and not by being present, absent or well formed. A token can record that a use began, or that
      the observation was made. It does not replace the observation.
    - *Unknown.* The history of the target is missing, unreadable, corrupt, incomplete or otherwise
      unresolved. That is treated as a previous use whose writers are not known to have ceased, and
      decision 34 refuses. This holds for the warm and for the cold target alike.

    None of the following establishes that a target was never used by a gate: the absence of a marker or of
    a record; the directory having been absent earlier, or having been recreated; the cooperative lease
    being free or held (the kernel frees it at once when a holder is killed while its descendants live); a
    recorded process id that is no longer present; an operator's acknowledgement. Creating a target that
    does not exist stays outside the four operations of decision 32, but the absence of the directory is not
    provenance: it does not show that no use began at that path, and it lifts no refusal that the
    provenance requires for an unfinished use recorded there.

    The provenance has to be durable and reviewed (the plan's prerequisite P5). Requiring it is part of this
    decision. Its implementation is not designed here and does not exist. For the warm target, the marker
    and the completed record of decisions 30 and 35 are the designed trace. They are a lifecycle trace:
    which valid provenance is required depends on the state that the use is in, as the clarification below
    says. Whether a later run may rely on them as read, and how they are protected, belongs to the review
    of that prerequisite; the open security finding on recovery contents used as authority is not closed
    here. The cold target has no trace at all in ADR 0013.

    The lead's binding clarification, accepted with this record and quoted word for word. In it, "D35" is
    decision 35, "P1" is the plan's name for the observation that this decision requires and "P5" its
    name for the durable provenance; "RC" and "TV" name the plan's recovery cases and vectors.

    > The marker and completed record form a lifecycle trace; the required valid provenance depends on the
    > lifecycle state. In a completed state, D35 intentionally removes the unfinished marker after
    > independently established cessation and completed-record publication. That intentional absence is not,
    > by itself, missing or corrupt provenance, and the completed state does not require both files
    > simultaneously. An unfinished state is not classified as never-used merely because it has no completed
    > record. Missing, unreadable, corrupt, incomplete or unresolved provenance required for the applicable
    > state refuses for both warm and cold targets. Neither a marker nor a completed record, by presence,
    > absence or well-formed content, independently proves cessation or authorizes recovery. Reliance on the
    > trace remains subject to P5's independent review and P1's accepted observation. P5 is unimplemented.
    >
    > All cases described as established, including RC1/3/5 reuse, RC8, TV14 and TV15 resets, require both P1
    > and P5's review of the trace before execution.

    Read with the three cases above: a completed state belongs to the second case only on the proof that
    the second case requires, and the marker that decision 35 removed does not move it to the third. An
    unfinished state is never the first case, and it stays refused under decisions 34 and 35 until the
    cessation of that use is established. Neither file is an authority for cessation in either state.

34. **Without established cessation the operation is refused, and the run does not qualify.** Unknown
    ownership is treated as a live writer, and unknown history (decision 33) as a previous use that has not
    ceased. The gate then does not publish, remove, reset or reuse; it leaves the target and the marker as
    they are, sends no signal to a process it cannot attribute, and says why, naming what it observed as a
    diagnostic. A wait for cessation is bounded. When it ends without proof, the outcome is this refusal: an
    expired wait is not permission, and neither is a process id that went missing during it. At the start of
    a run that needs a removal, a reset or a reuse, the refusal is a preflight refusal (`REFUSED`, exit 1, decisions 5
    and 18) and nothing builds. At the end of a run, the refusal leaves the target in place, unpublished.
    Recovery stays refused until cessation is established by decision 33's means; no later run, elapsed time
    or acknowledgement changes that.

    A run that ends with the cessation of its writers unresolved is not qualifying evidence (decision 17):
    not for a builder, a merge, a goal or a release, and not in any tier, `fast` included. The step results
    that were observed may be kept and shown. They cannot add up to a qualifying overall success. The result
    states `qualifying: no` with this reason, and its summary shows the target as kept and why
    (`target: kept (<reason>)`), in every tier. A later run that is refused because of that earlier use
    names the earlier run's identity and end time where validated diagnostics supply them, and says
    `unavailable` where they do not. It invents no identity, treats no marker as authority and does not
    count the disclosure as cessation. How the overall verdict and the exit status present such a run is
    left to the implementation's interface and its review. The floor in this paragraph is not.

35. **The warm record is published at established cessation, not at the end of the last command.** The
    marker and the source identities are written before the first building command, as decision 30 says.
    The record is written, and the marker removed, only when decision 32 is met for this use. That is so
    after a successful build, after a compile error and after a handled interrupt alike. The end time in the
    record is taken when cessation is established, not earlier: a writer that finished after an earlier end
    time could otherwise leave an artifact that looks newer than a source edit it never saw. A marker
    therefore survives every use whose cessation was not established, not only a gate that was killed. A
    run that finds a marker refuses (decision 34) unless the cessation of that earlier use is established;
    only then does it reset, with `reset: previous build did not finish`. This is the rule for every use
    that ended without established cessation, however it ended: a normal finish, a failed compilation, a
    handled interrupt or the death of the holder. No record was completed for such a use, so no later run
    reports `reused` for it. Once its cessation is established, the next run resets. Until then every later
    run is refused.

36. **Deleting the cold target and resetting the warm target are removals under decision 32.** Decision 14's
    deletion of `target/gate` before and after a run, its cleanup on failure or interruption, and decision
    15's resets keep their conditions and their reasons. Each is carried out only when decision 32 is met.
    A cold target that is left in place because cessation was not established is not a defect of the run
    that left it. A cold target removed under a live writer would be: the writer can recreate it, and the
    next cold run would start on a tree it believes is fresh. A cold target that was left in place is
    removed by a later run only once that cessation is established, and each later `full` run is refused
    until then. The deletion before a run is covered in the same way. A `target/gate` that exists when a
    `full` run starts, after `--keep`, after a killed run or from any other origin, is a possible previous
    use, and deleting it needs the affirmative provenance of decision 33. No reviewed durable provenance
    exists for the cold target, so that deletion is refused today. The plan's vector TV13, which expects a
    planted cold target to be deleted, keeps its expected result and is unmet until the provenance exists.
    It is not passing and it is not waived.

37. **The lease holder is the process the caller started.** It may have replaced itself by `exec`. It is the
    only owner of the lease's file descriptor. No helper keeps that descriptor after the holder is killed,
    and no descendant inherits it. A helper that outlives the holder does not change a target, because the
    next run may already hold the lease; a signal from such a helper needs authority from a child it started
    and has not yet reaped. Three contracts stay distinct, and none stands in for another: the kernel frees
    the lease at once when the actual holder is killed with live descendants; a handled INT, TERM or HUP
    ends in a bounded teardown; a measurement supervisor may hold a lease of its own through its teardown.

38. **No number is chosen for the grace period of a handled interrupt, or for any other bound of the
    teardown and of a wait for cessation.** The bounds are: the grace period of a handled interrupt; the
    wait for the gate's children after a signal; the wait for cessation at the end of a run, before a
    publication or a removal; and the wait for cessation at the start of a run that found an earlier use.
    Each is bounded and none has a value yet. The wait for the lease has its value in decision 21 and is not
    among them. The deadlines in the gate's test files are budgets of those tests. No product limit is
    taken from them, and none is stated as a supported property of the gate. The owner of the gate's
    implementation and of the release says whether these are distinct bounds or one shared bound, and
    supplies every supported value with its justification, once a workable route for decision 33 exists.
    Independent reviewers assess the whole set. Only then are the test budgets derived from it, not the
    reverse, and not from one of the bounds alone.

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

The effects in this table are in force since this record was accepted, and only for the clauses named. In
every other clause ADR 0013 stands as accepted.

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
| Decision 17 | The conditions under which a run qualifies as evidence | Qualified by decision 34: a run whose cessation is unresolved does not qualify. No word is replaced |
| Decision 21 | The holder of the lease | Qualified by decision 37. No word is replaced |

Every other decision, consequence and alternative of ADR 0013 is unchanged.

Before this record was accepted, the same points already bound as the lead's rulings recorded in the plan
(section 12: holds K-1 to K-5, the broader coupling, and the rulings carried by the sixth amendment). They
are now the decision record as well. Finishing the reviews of this record did not accept it; the lead
recorded the acceptance, with the clarification quoted in decision 33 (the plan, section 12, seventh
amendment).

## What this record does not do

It removes permissions that contradicted the rulings. It does not supply the observation that decision 33
requires. No reviewed way exists today to establish, on a macOS host where all agents share one account,
which processes belong to a run after a descendant is re-parented. Until one is established and accepted by
the security and resilience reviewers, the gate cannot publish, remove, reset or reuse a target there, and
the folded gate stays blocked. The measured baseline is a separate matter: the owner has since authorized
it as a simple, best-effort measurement of the unchanged earlier gate script under the cooperative lease,
with no containment supervisor (the plan, section 12, seventh amendment). That authorization lifts no hold
of this record, and its result is no evidence for any decision here. This record does not turn the block
of the folded gate into a passing state: a gate that always refuses does not meet decisions 14, 15 and 30,
the vectors that expect `reused`, a reset or a removed cold target, or the timing requirement of the warm
tiers. Whether native Linux offers such an observation has not been evaluated.

Not decided here:

- which mechanism establishes cessation, and on which hosts;
- the values of the bounds of decision 38, and whether they are one bound or several;
- the exact wording of the refusal;
- how the overall verdict and the exit status present a run whose steps passed and whose cessation was
  unresolved at its end. That such a run does not qualify, and that its result shows the kept target, is
  decided (decision 34);
- a fresh target path instead of a reset, and a dedicated account for gate runs. Neither was evaluated;
  neither is adopted or ruled out.

One prerequisite is load-bearing, and it is not implemented: the durable provenance of decision 33 (the
plan's P5). A later run must be able to know, affirmatively, whether a gate use ever began in a target and
whether that use reached established cessation. Decisions 33, 34 and 36 depend on it. Revision 1 of this
record said that its decisions did not; that sentence was wrong and is withdrawn. The requirement is
decided. Its implementation is not designed, chosen or started here. The marker and the record of decisions
30 and 35 are the designed trace for the warm target: a lifecycle trace, in which the provenance that is
required depends on the state (decision 33 and the clarification quoted there). The cold target has none
in ADR 0013 and needs one, because a killed or a kept `full` run must be recognizable to the next run.
Until a reviewed provenance exists:

- an existing cold target cannot be told apart from an unfinished use, so its deletion at the start of a
  run is refused, and the plan's vector TV13 is unmet;
- a warm target is refused in the same way when the provenance that its lifecycle state requires is
  missing, unreadable, corrupt, incomplete or unresolved. The marker that decision 35 removed when the
  completed record was published is not, by that absence, missing or corrupt provenance, and a use
  without a completed record is not thereby a target that was never used;
- no later run relies on the trace as read: every case that the plan describes as established, a reuse
  and a reset included, waits for the review of that provenance as well as for the observation of
  decision 33.

A limit that stays: the gate and its operator share one account. A marker deleted by hand can look to the
next run like a target whose last use had closed, and a history rewritten by hand can look like any of the
three cases of decision 33. No document offers that as a recovery, and the gate cannot detect it.

## Consequences

- The warm record now means "this tree was built here and its writers are known to have stopped". A reuse
  rests on that, not on the gate having outlived its direct children.
- A host without an accepted observation produces no reusable warm target and keeps its cold target. The
  price is paid in the open, as a refusal with a reason, not as a silent stale artifact.
- An existing target without affirmative provenance is refused. Until the durable provenance exists, a
  `target/gate` left by `--keep`, by a killed run or by anything else blocks the next `full` run on that
  worktree, and a warm target with a damaged record blocks `pr` and `fast` there. No document offers a
  recovery for that state yet.
- A run that kept its target is visibly not qualifying in every tier, the builder's included. A builder sees
  the reason in the run that caused it, not one run later as an unexplained refusal.
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
- **A31. Treat a target that no gate use is known to have produced as having no previous use.** This was the
  text of revision 1. Not knowing of a use is not knowing that there was none. A killed or a kept `full`
  run leaves a cold target with no trace, and the rule would delete it under a writer that may still live.
- **A32. Let a run whose steps passed qualify although its cessation is unresolved, or decide that per
  tier.** Such a run cannot say that its artifacts are final. On the builder's tier it would report a
  success whose price appears one run later, as a refusal with no visible cause.
