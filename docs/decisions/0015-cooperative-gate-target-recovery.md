# 0015. A use of a gate target is finished when the processes the gate started have exited under the cooperative lease

- Status: proposed. Decision 41 records a decision of the owner. It is binding from 2026-11-15, whatever
  the status of this record. Decisions 42–47 are the implementation contract derived from it. They are
  **proposed**: they await the check of this record's exact commit by the same security and resilience
  reviewers and the lead's ruling. Until the lead has ruled, no builder implements them.
- Date: 2026-11-15
- Qualifies [ADR 0014](0014-gate-target-recovery-and-selection-coupling.md). Decision 41 overrides, from its
  date, the clauses of ADR 0014 decisions 32–38 that demand independently established cessation of every
  writer. On acceptance, decisions 42–47 would supersede the clauses of ADR 0014 that are listed under
  "What this changes in ADR 0014", and only those. ADR 0014 decisions 37, 39 and 40 stand. No word of ADR
  0014 or of [ADR 0013](0013-tiered-verification-gates.md) is rewritten.

Origin. Decisions 41–47 were first written into ADR 0014 as a "revision 3" and were reviewed there at one
exact commit by the security and the resilience reviewer. Each passed that text with conditions. The
decision index asks for a new record when an accepted decision changes, so the lead ruled that the text
becomes this successor record. ADR 0014 is restored to the text accepted on 2026-11-14; only its status
line now points here. The decisions keep the numbers 41–47, the alternatives keep A33–A39, and the plan's
cases keep their names, so that every earlier reference still resolves.

Changes from the reviewed text. Each follows a finding of those reviews as the lead ruled on it. The
review ids are given so that a reader can find the finding. No mechanism is added and no number is
chosen.

- Decision 41 is unchanged, word for word.
- Decision 42: the lifecycle needs the configured cooperative lease. A run started without it is refused
  before it changes anything. A run that inherited the lease checks the inherited holder again at its
  finish (CR-7). The way out on a platform without the shown recovery no longer reads as an invitation to
  remove `target/` (CR-1).
- Decision 43: the marker's lock is pinned to a lock of the open file description, and the holder ends its
  hold only by closing, never by an explicit unlock (CR-2). "A finished use has a record" is said for the
  warm target only (CS-2). What a complete record is, and that it covers the exact set of source files, is
  defined (CR-4). The claim that a removed `target/` is always the ordinary case is withdrawn (CR-1).
- Decision 44: the holder checks at admission that the marker's path still names the file it locked
  (CR-3). The order now says what is synchronized to stable storage, and when (CR-4). The source
  identities are captured before the first build and again at the finish (CR-5). A failure of any
  operation of the order has a stated outcome (CR-6). The lock is closed, not unlocked (CR-2).
- Decision 45: the structural check covers the state files as well as the directories (CS-1). A signal to
  a process group has a stated end of authority (CR-12).
- Decision 46: rows for the lost lease, for a failed operation, for a source that changed during the build
  and for a run without the lease. The content floor of the refusal (CR-9).
- Decision 47: the removal of a target and the publication are named as work that needs a bound or a
  recorded decision (CR-6).
- "What this changes in ADR 0014": the first four sentences of the lead's clarification stand, not five;
  the fifth is superseded (CS-2).
- "Consequences": the sentences on a killed gate and on `cargo clean` are corrected (CR-1, CR-8).
- Alternatives A40–A43 are added.

## Context

ADR 0014 is accepted. Its decisions 32–38 let the gate publish, remove, reset or reuse a target only after
independently established cessation of every writer of the previous use, a detached descendant included,
on the evidence of a durable provenance. No reviewed observation of that kind exists on a macOS host
where every agent runs under one account, so the folded gate could not be built there. The lead asked the
owner what should happen with the rule. The owner answered "relax it". This record carries that answer
and the smallest contract that follows from it. The [plan](../plans/2026-11-14-tiered-checks.md) (section
12, eighth and ninth amendments) has the state table, the cases and the work that follows.

Facts this record rests on. Nothing was run for it.

- The gate script at the base commit sets its build directory to `target/gate`, deletes and recreates it
  at the start, and deletes it again from a handler for exit, INT and TERM. It runs its commands one
  after another and waits for each. It observes no other process. (Read in the script.) The owner's rule
  is therefore the guarantee the project has today, with the lease added.
- A kernel advisory lock of the kind the lease uses belongs to an open file description. A process that
  inherited the description keeps the lock after the process that took it has died. An explicit unlock
  through any descriptor of that description releases the lock for every process that shares it. A
  per-process record lock is a different kind: a child does not inherit it, and the process loses it when
  it closes any descriptor of the file. On some network filesystems a lock of the first kind is carried
  out as one of the second. This is the documented behaviour of these locks on both platforms. None of it
  has been shown in this project: not for the file lock of the pinned toolchain, and not that the real
  `cargo` keeps an inherited descriptor until it exits. All of it is an assumption until the plan's case
  RC18 has run.
- `cargo clean` deletes the whole `target/` directory, and with it every file the gate keeps there.
  (Documented behaviour of cargo; not run.)
- The reviews of a preliminary registry model found two defects that follow from keeping a second,
  independent memory of a target: after `cargo clean` the registry and the disk contradict each other and
  every later run is refused, with no way out; and the model bound the identity of a target directory
  before the directory could exist. (Retained review findings. The model is kept outside this record, is
  not integrated, and its open findings are not repaired or closed here.)

## Decision

The numbers continue those of ADR 0013 (decisions 1–31) and ADR 0014 (decisions 32–40), so that a decision
number names one decision across the three records. "A target", "a use" and "a writer" are as ADR 0014
defines them. *The holder* is the gate process of decision 37. *A directly started process* is a process
that the holder itself created for a step of the run, and whose exit status the holder collects through
its own handle: `cargo`, `git` and the other commands the gate runs. A process that such a command starts
is not one. *A descendant* is any process further down, whatever became of its parent.

Decision 41 is binding. Decisions 42–47 are proposed (see the status line).

41. **The owner's decision: a use is finished when the processes the gate itself started have exited and
    the gate holds the cooperative lease.** The question put to the owner was what should happen with the
    rule that a target may be reused, reset or removed only after proof that every process of the previous
    use has stopped, including a process that detached itself from the gate. The owner was told that
    today's gate script deletes and recreates `target/gate` and waits only for the commands it started,
    with no such proof. The owner's words, verbatim:

    > relax it

    Recorded with that answer: a use of a target counts as finished when the processes the gate itself
    started have exited and the gate holds the cooperative machine lease. Proof that escaped, detached or
    re-parented descendants have stopped is not required, on macOS or on any other platform. The decision
    overrides the clauses of decisions 32–38 that demand independently established cessation. It overrules
    the security finding on ownership and signal authority (review id FS1) as a block on the folded gate.

    It is a decision about a risk, not a repair. FS1 is recorded as a risk the owner accepted. It is never
    reported as resolved, fixed or passing. The accepted cost: a process that the gate did not start
    itself, and that a test or a build script left behind, can still write into a warm target after its
    record was published, or into a cold target while it is removed or after it was recreated. A later run
    may then trust what that process wrote. Nothing in this contract detects it.

    The decision removes the cessation requirement and nothing else. It waives no test and no review of
    the folded gate. It changes no other decision of the owner: one green Linux run at the final head of a
    pull request, the nextest qualification, the cooperative lease, and that the owner alone changes the
    repository rules and merges.

42. **Who observes that a use is finished.** There are two observers and no third.

    *The lease.* Both observations need the configured cooperative lease of decision 21: the lease at the
    default path, or at the path the run was configured with. A run holds it in one of two ways. It took
    the lease itself, in the mode its tier takes. Or it inherited the lease from a holder that started it
    and that is validated as alive and still holding the exclusive lock (the plan's obligation RS7). A run
    that was started without the lease (`--no-lease`) holds it in neither way. This lifecycle is not
    supported for such a run. It is refused before admission: it creates no marker, resets nothing, builds
    nothing, publishes nothing and does not qualify. The approved plan starts the hosted CI job with
    `--no-lease`. How that job is to use a gate target is not decided here. It is an open obligation of
    the pull request that changes CI, to be reconciled and reviewed there. This record does not withdraw
    that direction of the owner, and it does not let a run proceed on the marker's lock alone.

    *The holder, for its own use.* The use is finished when all of these hold: every directly started
    process of the use has exited and the holder has collected its exit status; the holder holds the lease
    as said above and has not recorded it as lost; where the lease was inherited, the check of the
    inherited holder is made again at the finish and passes; and the holder holds the lock of its use
    marker (decision 43), at a path that still names the file it locked. The exit status decides the
    verdict of the step. It does not decide whether the use is finished: a command that failed has
    exited, and a use that is finished has not thereby passed.

    *A later run, for an earlier use that did not finish.* Such a use has left its marker (decision 43).
    The later run holds the lease and then acquires the lock of that marker. The earlier holder gave an
    open description of that lock to every process it started directly. The kernel grants the lock to the
    later run only when no process holds that description any more. That grant, under the lease, is the
    observation that the directly started processes of the earlier use have exited. Until it is granted
    the earlier use is unfinished, and the later run changes nothing.

    None of these is an observation of either kind, alone or together: the lease being free, or being
    held (the kernel frees it at once when a holder is killed while processes it started still run); a
    recorded process id that is no longer present; a listing of processes or of open files, whether it
    shows something or nothing; the end of a bounded wait, or any elapsed time; a marker being present or
    absent, or what it contains; a record, or what it contains; end of file on a channel; an operator's
    acknowledgement.

    Three limits of the second observation are stated here and are not hidden:

    - It shows that no process holds the description. A directly started process that closed the
      description and kept running would not be seen. The gate starts a short, fixed list of commands. For
      each of them, on each platform and on each kind of filesystem that is to be supported, the
      implementation shows that the command holds the description until it exits and that the lock behaves
      as decision 43 requires (the plan's case RC18). Where that is not shown, this recovery is not
      supported: a marker found at admission is refused there, whether or not its lock is granted. The
      only way out there is the operator's. The operator makes sure, by the operator's own means, that no
      process of the earlier use is still running, and only then removes the worktree's `target/`. The
      gate cannot check that. It does not treat the removal as an observation, and its refusal says not
      to remove `target/` while such a process is alive (decisions 43 and 46).
    - It says nothing about a process the gate did not start itself that dropped the description. That is
      the risk of decision 41.
    - A descendant that kept the description keeps the lock. The gate cannot tell it from a directly
      started process, so it refuses. That is conservative. It is not relied on as proof about
      descendants, and it ends when that process exits.

43. **The local state is a use marker and a completed record. There is no registry.** For each target the
    gate keeps its state in one gate-owned directory inside the worktree's `target/` and outside both
    targets. The file names are the implementation's.

    - *The use marker* exists exactly while a use is unfinished. The holder creates it, or takes over the
      marker that an unfinished use left, and locks it before it examines or changes anything in the
      target. Taking over means locking the file that is there; the marker is not replaced. The lock is
      the single-writer lock of the target. An open description of it is held by the holder and by every
      process the holder starts directly for the use, and is used for nothing else. The holder removes
      the marker as its last act on the target, when the use is finished. What the marker contains (the
      run's identity, its start time, the process ids of the directly started processes) is a diagnostic.
      No decision reads it. If it cannot be read, the diagnostic is reported as unavailable; the marker is
      still not replaced, and its lock decides as before.
    - *The kind of lock, and how it ends.* The lock is a kernel advisory lock that belongs to the open
      file description, the kind decision 21 uses. It is never a per-process record lock. The holder
      never unlocks it explicitly, on any path: not at a finish, not when it keeps the target, not when
      the lease was lost and not on an error. It ends its own hold only by closing its own descriptor, or
      by exiting. On the finished path it removes the marker first and closes afterwards. An explicit
      unlock would free the lock for every process that shares the description, and a later run would
      then reset under a directly started process that is still running. The lock is relied on only on a
      kind of filesystem where it has been shown to behave this way, including after the death of the
      process that took it (RC18). Any other kind, and a kind the gate cannot determine, is the
      unsupported case of decision 42: a marker found at admission is refused. This record does not claim
      that the file lock of the pinned toolchain, or any filesystem, meets these conditions. That is
      evidence still to come.
    - *The completed record* belongs to the warm target only. It holds what decisions 15 and 30 of ADR
      0013 say: the canonical worktree root, the toolchain identity, a content identity of every tracked
      and untracked source file, and the end time. It is written to a temporary file and renamed. The cold
      target has no record, because it is deleted at the start of every `full` run whatever its history.
    - *A complete record.* A record is complete only if its encoding makes any truncation detectable, so
      that a shortened record cannot be read as a whole one, and if it is read within the limit of
      decision 45. That a record parses is not enough. The record covers the exact set of source files:
      for the comparison below, a source file that exists now and that the record does not list is a file
      whose identity differs, and so is a file that the record lists and that no longer exists. The
      encoding and its limits are the implementation's, and its review assesses them.
    - The marker and the record are the lifecycle trace of the lead's clarification (ADR 0014, decision
      33). A finished use of the warm target has a record and no marker. A finished use of the cold
      target has neither: its target was removed, or was kept by `--keep`, and the next `full` run deletes
      what is there. An unfinished use of either target has a marker.
    - The state is local, and it is deleted together with `target/`. Nothing outside the worktree
      remembers a target, so no earlier state can contradict a `target/` that was removed, by `cargo
      clean` or by hand, and no run is refused for want of a history. That removal is the ordinary case
      of an absent target only if no directly started process of an earlier use was still running when
      it happened. The gate cannot establish that once its marker is gone. If the holder was killed, a
      command it had started is still running, and an operator then removes `target/`, the next run sees
      an absent target and builds beside that command. This is a residual of this contract. It concerns a
      directly started process, so it is not the risk of decision 41 and is not accepted as an extension
      of it. The removal is no proof that the process exited. The contract discloses the residual and
      steers away from it (decisions 42 and 46); it does not close it. This record does not say that
      `cargo clean` is always safe.
    - Metadata is data, not authority. Neither file names what to delete or what to signal, and neither
      is evidence that a directly started process has exited. A record can only withhold a reuse. A
      record or a state that is missing, unreadable, truncated, corrupt, incomplete or inconsistent never
      authorizes a reuse. It does not refuse the run either: the target is reset (decision 45). A reuse
      needs all of: no marker; a complete record; the same canonical worktree root; the same toolchain
      identity; and no file whose identity differs from the record while its modification time is not
      later than the recorded end time. A listed file that no longer exists has no modification time and
      withholds the reuse. There is no fallback to an older record or to a copy, and a leftover temporary
      file is never read as a record.

44. **The order of a use.** The lease comes first, as decisions 21 and 42 say. A run without it is
    refused here, before step 1. Then:

    1. *Admission.* The holder checks the structure of `target/`, of the state directory and of the state
       files (decision 45). It acquires the lock of the use marker, creating the marker exclusively if
       there is none. If the lock is not granted, the run is refused (`REFUSED`, exit 1, never the exit
       status of lease contention), says what decision 46 requires, and changes nothing. A wait for the
       lock is bounded, and its end without the lock is the same refusal. Once the lock is granted, and
       before the holder examines or changes anything in the target, it checks that the marker's path
       still names the file it locked. If it does not, the run is refused and changes nothing; the holder
       closes its descriptor and does not try again in the same run. If the marker was there before this
       run, an earlier use did not finish, and step 3 resets, where that recovery is supported (decision
       42). Before step 3 changes anything, the marker and the directory that holds it are synchronized
       to stable storage.
    2. *Structure.* The holder checks the target as decision 45 says. On a failure the run is refused and
       nothing is deleted.
    3. *Decision.* Cold: an existing `target/gate` is deleted. Warm: reuse on the conditions of decision
       43, otherwise reset, with the reason. A leftover temporary file of an earlier publication is
       removed here and is never read.
    4. *Invalidation before the build.* Before the first building command the completed record of the
       earlier use is removed, and the state directory is synchronized to stable storage. A marker that
       someone deletes by hand then leaves no record that could revive a reuse. If the marker cannot be
       created or the record cannot be removed, nothing builds.
    5. *The commands.* For the warm target the source identities are captured before the first building
       command. Each directly started process receives the description of the marker's lock. The holder
       waits for each and collects its exit status.
    6. *Finish* (decision 42). The end time is taken now. For the warm target the source identities are
       captured again and compared with those of step 5.
    7. *Publication.* Warm, when the two captures agree: the record, with the identities and the end
       time, is written to a temporary file that is created exclusively; the temporary file is
       synchronized; it is renamed to the record's name; the state directory is synchronized; and only
       then is the marker removed. Warm, when the two captures differ in any file or in the set of files:
       no record is written, the marker is removed, and the result says that no record was published and
       why. The next run then finds no record and resets. A record never describes source that changed
       while it was being built. Cold: `target/gate` is deleted unless `--keep` was given, and then the
       marker is removed.
    8. The holder closes its descriptor of the marker and releases the lease. It does not unlock the
       marker (decision 43).

    A holder that dies anywhere between steps 1 and 7 leaves the marker, and the next run takes the
    recovery of decision 42 and resets. That covers a death while the target directory was being created
    (the marker exists and the target is absent or partly created), a death during a build, and a death
    after the record was renamed and before the marker was removed. In the last case the marker decides:
    the next run resets and does not report `reused`. The price is one rebuild after that crash.

    *When an operation of this order fails.* The operations are: creating or synchronizing the marker;
    removing a record or a temporary file; deleting or creating a target; writing, synchronizing or
    renaming the record; synchronizing the state directory; removing the marker. On an error from any of
    them the run stops and says which operation failed, and on which path. It does not repeat the
    operation in a loop. It publishes no record and falls back to no earlier one. It leaves the marker
    where there is one. If the failure comes before step 3 has changed anything, the run is refused and
    nothing is deleted or built. If it comes later, the result shows `target: kept (<reason>)` and
    `qualifying: no` with that reason, and a target that was partly deleted may stay partly deleted. The
    next run finds the marker and takes the recovery of decision 42.

    *After a loss of power.* The order is chosen so that the state on disk afterwards is one of three.
    A marker is there: the next run resets. No marker and no record: the next run resets. No marker and
    a complete record that was synchronized before the marker was removed: a reuse on the conditions of
    decision 43. A removal of the marker that had not reached stable storage brings the marker back and
    costs one rebuild. No power-loss case is exempt from this. Which calls synchronize a file and a
    directory on each platform, and whether they are available without a new dependency, is for the
    implementation to show and for its review to assess. A synchronization that cannot be achieved on a
    platform is brought to the lead. It is not dropped.

45. **The authority to delete comes from the structure of the path, never from data.** A reset or a
    removal deletes exactly one path: the constant name of the target, joined to the canonical worktree
    root that the gate computed itself. Before every deletion and before every build the gate checks that
    `target/`, the state directory and the target are each either absent or a real directory: not a
    symbolic link and not a special file, owned by the account the gate runs under, inside the worktree
    and on the worktree's filesystem. The deletion follows no symbolic link and does not cross to another
    filesystem. On any failure of these checks the run is refused, nothing is deleted, and a file outside
    the target is untouched. No path, identity or process id that was read from a record, from a marker or
    from the environment is deleted, followed or signalled.

    *The state files are part of the same boundary.* The marker, the record and a temporary file of a
    publication are each either absent or a regular file: not a symbolic link, not a FIFO, a socket, a
    device or a directory; owned by the account the gate runs under; with permissions for that account
    only. The gate opens none of them through a symbolic link, and the check does not block on a special
    file. A name that holds anything else is a failure of the structural check: the run is refused, the
    name is not opened for writing, not truncated, not replaced and not removed, and a file outside the
    state directory is untouched. The gate creates the marker and every temporary file exclusively, so
    that an existing name is never written through. It never truncates an existing state file through
    its path. A record is replaced only by the rename of step 7, and the marker's content is written only
    through the descriptor that the holder checked and locked. Every read of a marker or of a record is
    bounded by a limit that the implementation fixes; content beyond the limit makes a record damaged
    and a marker's diagnostic unavailable. A record that is a regular file with damaged content is not a
    failure of the structural check: the target is reset (decision 43). A marker whose content is
    damaged is not replaced and its lock is not bypassed: while the lock is not granted the run is
    refused, as with any marker. Which calls achieve this without a new dependency, and the limits
    themselves, are for the implementation and its review.

    The gate sends a signal only to a directly started process that it has not yet reaped, through its
    own handle, or to the process group that the holder itself created for such a process. A group may be
    signalled only while its leader is that directly started process and the holder has not yet reaped
    it. An escalation to the group is therefore sent before the leader is reaped, and the reaping ends
    the authority over the group, because its id can then name other processes. The implementation has
    to make a signal to a group whose leader was reaped impossible. If it cannot, it signals through the
    direct handle only. What a leader that has exited leaves behind in its group is the risk of decision
    41 and not a kept target. The gate signals no process id taken from a file or from a listing of
    processes, no process chosen by its parent, no process chosen by its account, and no process it
    learned of through an inherited descriptor.

46. **The endings of a use.** "Reaped" means that the holder collected the exit status. In every row but
    the last the lease is held as decision 42 requires.

    | The use ended by | What the holder observed | State left | Result, and the next run |
    |---|---|---|---|
    | A normal finish | Every directly started process reaped | Warm: record, no marker. Cold: target removed (or kept by `--keep`), no marker | The steps' verdicts. Next warm run on unchanged source: `reused`. Next `full`: deletes what is there |
    | A failed compilation | The same; a command exited with a failure | The same | `FAILED` on that step. Next warm run on the same source: `reused` |
    | A handled INT, TERM or HUP; the directly started processes exit within the bound | The holder signals them (decision 45) and reaps each | The same | `INTERRUPTED`; the lease is released. Next `pr` run: `reused` |
    | A handled INT, TERM or HUP; a directly started process has not exited when the bound ends | Not finished | The marker stays. No record. `target/gate` is left | `INTERRUPTED`, naming the processes that did not exit; `target: kept (<reason>)`; `qualifying: no`. Next run: the row "The next run after an unfinished use" |
    | A source file changed between the two captures of decision 44 (warm) | Every directly started process reaped; the captures differ | No record, no marker | The steps' verdicts; the result says that no record was published and why. Next warm run: `reset: no record` |
    | The lease was lost, or an inherited holder is no longer alive and holding the lock at the finish | Not finished | The marker stays. No record. The target is left | `target: kept (<reason>)`; `qualifying: no`. Next run: the row "The next run after an unfinished use" |
    | An operation of decision 44's order failed after the target was changed | Not finished | The marker stays. No record. The target is left as it is, possibly partly deleted | The operation and the path are named; `target: kept (<reason>)`; `qualifying: no`. Next run: the row "The next run after an unfinished use" |
    | The death of the holder: SIGKILL, or any signal it does not handle | Nothing; the holder is gone | The marker stays, and its lock is held by whichever processes still hold the description: directly started ones that are still running, and any descendant that inherited it. No record. The kernel has freed the lease | No result from that run. Next run: the row below |
    | The next run after an unfinished use | The lease, then the lock of the marker | While the lock is not granted: nothing changes. Once it is granted, where the recovery is supported: the target is reset or removed, and the run goes on | Not granted, or the recovery is not supported there: `REFUSED`, exit 1, with the content below, no signal; and again on every later run, however much time has passed. Granted and supported: warm `reset: previous build did not finish`; cold deleted and built from nothing. Never `reused` |
    | No use is known: an existing target with no marker and with a record that is missing or damaged, a target that a plain cargo command built, or a `target/gate` present at the start of `full` | The lease, then the lock of a new marker | Reset or removed | Warm `reset: <reason>`; cold deleted. Never `reused`, and not refused for want of a history |
    | No target: `target/` or the target was removed, with or without the state | The lease, then the lock of a new marker | Created | Built from nothing. This is correct only if no directly started process of an earlier use was running when the target was removed, which the gate cannot see (decision 43) |
    | The run was started without the lease (`--no-lease`) | Nothing; the lifecycle is not entered | Nothing is created, changed or deleted | `REFUSED`, exit 1, before admission; no record; `qualifying: no` |

    *What the refusal for a marker says.* The refusal for a marker whose lock is not granted, and for a
    marker on a platform where the recovery is not supported, contains at least: the absolute path of the
    marker; the marker's diagnostics, marked as not validated, or `unavailable`; that the target is in use
    or that an earlier use did not finish, because the lock cannot tell a running gate from the processes
    of a dead one; a hint for inspecting, read-only, which processes hold the marker's file; and the
    plain statement that `target/` must not be removed, by `cargo clean` or by hand, while any of them is
    alive. A listing of processes or of open files is a diagnostic for the operator. It gives the gate no
    authority to signal or to reset, and a listing that shows nothing is no proof that a process exited.
    The fixed strings are the implementation's interface (the plan's prerequisite P7).

    A run whose own use is not finished when it ends publishes nothing and removes nothing. That is the
    case when a directly started process had not exited at the end of the bound, when the lease was lost
    or an inherited holder was gone, when an operation of decision 44's order failed after the target was
    changed, or when the marker's path no longer named the file the holder had locked. Such a run does not
    qualify as evidence in any tier, and its result shows `target: kept (<reason>)` and `qualifying: no`
    with that reason. This keeps the floor of decision 34 for the cases that are left. A normal finish
    with every directly started process reaped is a finished use: there is no kept target in that case,
    also when a descendant survived. That is the risk of decision 41, not a case the gate reports.

47. **The bounds, and what is not chosen.** Decision 38 stands: no number is chosen here. The set of
    bounds changes in one place. It is now: the grace period of a handled interrupt; the wait for the
    directly started processes after a signal; and the wait for the lock of the use marker at admission,
    which may be no wait at all. The wait for cessation at the end of a run is withdrawn as a bound of its
    own: at a normal finish and after a failed compilation the holder's waits on its own commands are the
    wait. The owner of the gate's implementation and of the release says whether the three are distinct,
    and supplies every supported value with its justification. Independent reviewers assess the set, and
    only then are the budgets of the test files derived from it.

    Two more kinds of work belong to the same inventory, because a run cannot end before they do: the
    deletion of a target (a reset, the removal of the cold target at the start and at the end of a run,
    and after an interrupt), and the publication with its synchronizations. Neither is exempt. For each,
    the owner of the implementation and of the release proposes how it is bounded, or records why it can
    only be defined by its failure (decision 44), and the same reviewers assess that before any launch. A
    removal that cannot fit a supported bound is returned for a decision, as the earlier ruling in the
    plan says. This record does not claim that a blocking filesystem call can be cut off at a bound, and
    it states no value.

    The mechanism by which the holder handles INT, TERM and HUP is not chosen here either (the plan's
    prerequisite P2). The safety of a target does not depend on it: a signal that the holder does not
    handle is the death of the holder, in decision 46.

## What this changes in ADR 0014

ADR 0014 is not rewritten. The owner's decision (decision 41) is in force: the gate is no longer required
to establish the cessation of a process it did not start itself. Everything else in this table rests on
decisions 42–47 and is proposed. Until this record is accepted, no builder acts on a row, and a clause of
ADR 0014 that decision 41 does not override is still the accepted text. A clause that is not named
stands.

| Where in ADR 0014 | Clause | Under this record |
|---|---|---|
| Decision 32 | Publication, removal, reset and reuse each need "independently established cessation of every writer of the previous use"; "There is no exemption for a normal finish" | Superseded by decisions 41 and 42: each needs the use to be finished as decision 42 says. The four operations, both targets and every ending stay covered. "Creating a target that does not exist is none of the four operations" stands |
| Decision 33 | The observation that covers "every process the use started, directly or indirectly" and must not rest on their cooperation | Superseded by decisions 41 and 42 |
| Decision 33 | The list of what does not establish cessation | Stands, with one change: the holder's own reaping of the processes it started directly is now what finishes its use. End of file on a channel, a missing process id, the marker, the end of a wait, a handled signal and an operator's acknowledgement still establish nothing |
| Decision 33 | The three cases of affirmative provenance; "unknown" is refused; the durable provenance (the plan's P5) | Superseded by decisions 43, 45 and 46: an unknown history never authorizes a reuse and leads to a reset. No registry is required |
| Decision 33 | The lead's clarification of the lifecycle trace | Its first four sentences stand, with "independently established cessation" in the second read as "the use is finished". Its fifth sentence, that missing, unreadable, corrupt, incomplete or unresolved provenance "refuses for both warm and cold targets", is superseded by decisions 43–46: such a state is reset and never reused, and the refusals that are left are a marker whose lock is not granted, an unsupported recovery, a run without the lease and a failed structural check. Its sixth sentence, "Neither a marker nor a completed record … independently proves cessation or authorizes recovery", stands, and with it the rule that no metadata is evidence that a directly started process has exited: the authority is decisions 42 and 45. "Reliance on the trace remains subject to P5's independent review and P1's accepted observation", "P5 is unimplemented" and the sentence that the established cases need P1 and P5 are superseded: the reliance is subject to the review of decisions 42–47 and of their implementation |
| Decision 34 | Unknown history is treated as a previous use that has not ceased; the refusal for it | Superseded by decisions 43 and 46 |
| Decision 34 | A bounded wait that ends without the observation is a refusal; no signal to a process the gate cannot attribute; recovery does not decay with time or with an acknowledgement | Stand, with the observation of decision 42 |
| Decision 34 | A run with unresolved cessation does not qualify in any tier and shows the kept target; a later refusal names the earlier run from validated diagnostics or says `unavailable` | Stand for a use that is not finished (decision 46) |
| Decision 35 | The record is published, and the end time taken, "at established cessation" | Superseded as to *when*: when the use is finished (decisions 42 and 44). That the marker and the identities precede the first build, that a marker survives every unfinished use, and that such a use is never reported as `reused` stand |
| Decision 36 | An existing `target/gate` is refused at the start of `full` until durable provenance exists; the plan's vector TV13 is unmet until then | Superseded by decisions 44–46: it is deleted, under the lease and the marker's lock. TV13 becomes executable with the folded gate. It is still unrun |
| Decision 36 | Deleting the cold target and resetting the warm one are removals; a cold target left by an unfinished use is not a defect of that run | Stand, under decision 42 |
| Decision 37 | Whole | Stands. The description of the marker's lock is not the lease: the lease's descriptor is still owned by the holder alone and inherited by no process |
| Decision 38 | The four bounds | Changed by decision 47. That no number is chosen, and that no product limit is taken from a test's deadline, stand |
| "What this record does not do" | The folded gate is blocked until an observation of every descendant is accepted; the durable provenance is load-bearing; its three consequences | Superseded. The folded gate is no longer blocked by that. It is still not built, not reviewed and not run |
| "Consequences" | A host without an accepted observation produces no reusable warm target; an existing target without provenance blocks the next run, with no recovery | Superseded |
| Alternatives A23, A24 and A31 | Rejected because a descendant could still be writing | Overtaken by decision 41 to the extent of a process the gate did not start itself. The reasons stay on record as the description of the accepted risk |
| Alternatives A25, A26, A27, A32 | Rejected | Stand |
| The title | "…only after their writers have ceased" | Kept. "Ceased" is read as "the use is finished" |

In ADR 0013 the effect is this. The clause of its decision 30 that the record is written "once its
children have exited" was superseded by decision 35 and is not revived as written: decisions 42 and 44
say when the record is written, and they add the lease, the marker's lock and the order of invalidation.
The qualification of its decisions 14, 15, 17 and 21 by ADR 0014 continues, read through decisions
41–46. No word of ADR 0013 is replaced, and the table "What this changes in ADR 0013" in ADR 0014 is
unchanged.

## What this record does not do

It does not make the folded gate valid evidence. Nothing was built, run, probed, signalled or measured
for it. It closes no finding of the preliminary provenance model. It chooses no signal mechanism and no
number. The vectors that plant a stale artifact, write into `HOME`, start a shimmed CLI or run zero tests
keep their expected results, and so do the comparison of the folded with the ordinary run, the timing
targets, the equality of test names and counts, the evidence on each platform and the nextest
qualification. A reviewer's finding outside the risk of decision 41 binds as before.

Not decided here:

- the file names, the encoding of the record, the limits of a read, and the calls that open, lock,
  synchronize and delete: the implementation's, each with its own review;
- on which platforms, kinds of filesystem and commands the recovery after the death of the holder is
  supported: that follows from the evidence of RC18 and is not assumed;
- how the description of the marker's lock reaches a directly started process. If it is handed over as a
  standard stream, the stream that the suite's commands see differs from that of an ordinary run. That
  difference is then disclosed, and the comparison of the folded with the ordinary run uses the same
  stream on both sides or diagnoses the difference. Whether that route works without a new dependency is
  unverified;
- the fixed text and the exit status of a kept target and of the refusals;
- the values of the bounds of decision 47, and how the removal of a target and the publication are
  bounded;
- how the hosted CI job, which the approved plan starts without the lease, uses a gate target.

## Consequences

- The warm record means "this tree was built here, the commands the gate started for it have exited, the
  source did not change while they ran, and the gate held the lease". It no longer says that every
  writer is known to have stopped.
- A process that a test or a build script left behind can write into a target that a later run trusts.
  The owner accepted that (decision 41). The documents that describe the tiers have to say it.
- A killed gate blocks its worktree for as long as any process holds the description of the marker's
  lock: a command the gate started directly, or a descendant that inherited the description. After that
  the next run resets by itself, where that recovery is supported. Where it is not, the worktree stays
  refused until the operator has removed `target/`.
- A damaged record, a missing record and a target removed by hand cost one rebuild and block no run.
  `cargo clean` does too, but only when no command of an earlier use is still running. If one is, the
  next run builds beside it and nothing reports that. The refusal that precedes such a removal says not
  to do it.
- The gate hands a descriptor to the commands it starts, and that descriptor bears on safety: holding it
  is what keeps a later run out. The refusal has to say where the lock is, so that the operator can find
  the process.
- A source edit during a warm build costs the next run a rebuild from nothing, because no record is
  published for that build.
- A run without the lease cannot use a gate target. The hosted CI job of the approved plan is such a run,
  so the pull request that changes CI has to settle how it gets a target before it can land.
- The gate script's unconditional deletion of its target at the start, and its handler that deletes the
  target on exit, INT and TERM, both have to go when the folded gate replaces them, whether or not the
  holder is started by `exec`. Left in front of the holder, either would delete a target while a command
  of the holder is running. That is a change of code for a later pull request. Nothing is edited now.
- The frozen test files expect, in places, the refusals of ADR 0014 as accepted, and one of them
  contends for a target with two runs that have no lease. The plan lists the edits; none is made or
  authorized by this record.

## Alternatives rejected

- **A33. After the death of the holder, let the lease alone authorize the reset.** The kernel frees the
  lease while the processes the holder started are still writing. The owner's rule asks for their exit as
  well.
- **A34. Record the process ids of the directly started processes and reset when they are gone.** A
  process id is not an identity (A26).
- **A35. Reset after a wait.** Time that passed observes no process.
- **A36. Refuse for ever after the death of the holder, as the general rule.** Every killed run would
  block its worktree until someone removed the state by hand. That stays only as the fallback on a
  platform where the second observation of decision 42 cannot be shown.
- **A37. Keep an independent registry of targets and generations.** Its purpose was to prove what the
  owner no longer requires. It also turned `cargo clean` into a contradiction between the registry and
  the disk that nothing could resolve, and it bound a target's identity before the directory could exist.
- **A38. A separate writer lock that no process inherits, beside the marker.** Two locks would need an
  order and a second recovery. The marker's lock already excludes a second gate.
- **A39. A fresh target path after an unfinished use, instead of a reset.** It changes the fixed target
  paths of the plan, leaves directories of several gigabytes behind, and still needs the same
  observation before one of them may be deleted. Not adopted.
- **A40. Keep the use marker outside `target/`, where `cargo clean` cannot reach it.** It would close the
  residual of decision 43 for `cargo clean`. Inside the worktree it needs a new ignored path. Outside it,
  it leaves the boundary of decision 45 and needs structural checks of its own, and the way out on an
  unsupported platform becomes the removal of that state. Not adopted now. It is the stronger remedy if
  the residual is later judged too large.
- **A41. Let a run without the lease reset and build on the marker's lock alone, and never qualify.** The
  owner's rule names two conditions, and the lease is one of them. That such a run does not qualify does
  not make its use finished.
- **A42. At the finish, probe the marker's lock to report a descendant that still holds the
  description.** It would make the risk of decision 41 visible. It is a diagnostic and no protection, and
  the contract does not need it. Not adopted now.
- **A43. Retry admission when the marker's path no longer names the locked file.** A refusal is simpler
  and changes nothing; the next run starts clean. A retry would add a loop that needs a bound of its own.


## Native implementation dependency decision (2026-11-15)

The normal-development execution amendment in the spec/plan supersedes historical
execution preconditions only; D45 and unsafe_code=forbid remain intact. The lead's
scoped continuation permits xtask to use workspace nix at the already resolved
0.31.3, with `fs`, `dir` and `user` features selected: safe descriptor-relative
open/stat/inspection, native directory iteration, and effective-account identity. No
version upgrade or alternative crate is authorized. Safe openat/fstatat/mkdirat,
renameat/unlinkat, directory iteration and synchronization APIs are evaluated
against D45; an API's existence alone proves no boundary. Missing native
capability remains a refusal. RC18 is qualified only by actual retained native
command/filesystem evidence, never a filesystem-name allowlist.

ARCHITECTURE section 4.8 check: nix is the maintained Unix syscall wrapper already
selected in the workspace; its pinned source declares Rust 1.69 MSRV (below the
pinned 1.99.1) and MIT licence (allowed by deny.toml). Reuse avoids handwritten
unsafe syscall/ABI wrappers. Feature selection and exact native evidence are
recorded in the F2 handoff; no product runtime dependency is added. This does not
claim whole-gate or cross-platform qualification.


### Native capability result (F2 continuation; incomplete)

Read-only inspection now opens the canonical root one component at a time from
`/`, then opens `target`, the state directory and the constant build directory
relative to checked descriptors with no-follow/nonblocking flags. State leaves
are inspected without following links. Ownership, root device, private regular
state type/mode/link count and opened inode identity remain required. The target
inventory is bounded and does not follow artifact symlinks. Existing-directory
synchronization retains the pinned std implementation's errors (no weaker Apple
fallback). This is inspection evidence only, never authority to delete.

The scoped safe API route does not provide an atomic operation that conditions
unlink/rename on the checked inode, or a mechanism that pins an opened directory's
ancestry. Native private-fixture interleavings demonstrate (1) `unlinkat` removes
a replaced marker name while the checked original stays locked, (2) `renameat`
can publish a replaced temporary name, and (3) an opened directory remains usable
after relocation outside the constant target, where relative unlink can remove
a newly created outside-target canary. Checks immediately before these calls do
not close the final interval. No production destructive wrapper is introduced.

Admission retains an explicit capability refusal before mkdir, marker creation,
locking, target deletion or build. Native reset/cold deletion and successful F2
admission remain incomplete. A namespace-stability assumption would change this
boundary and is not adopted here. RC18 remains unsupported in production: the
prior one-command observation plus these inspection controls is not qualification
of the gate's fixed command list on a platform/filesystem. Native Linux remains
unrun. Existing owner-set-aside execution findings and owner-accepted FS1 risk
remain distinct from this product capability failure.


## F3 implementation proposal (2026-11-15)

This is a proposed complete D47 value set for final whole-gate review, not an
accepted ADR or whole-gate readiness claim. Ordinary development execution is
already authorized; historical per-slice launch conditions above are superseded
as preconditions by the normal-development amendment. D45 remains unchanged.

| Work | Proposed supported value / outcome | Reason |
|---|---|---|
| Grace after observing handled INT/TERM/HUP | 2 seconds from forwarding the first signal to the unreaped direct child | Gives ordinary command cleanup an opportunity without letting a TERM-ignoring command hold iteration indefinitely |
| Wait after KILL | 3 seconds from the first KILL attempt, shared by normal error and Drop paths; no fresh allowance on retry/Drop | Poll `try_wait` through the exclusive owned Child; only an actually collected status establishes reaping. An unreaped child means incomplete teardown, unknown status, kept target/marker, no publication |
| Marker lock at admission | 0 seconds (`try_lock` once), REFUSED exit 1 if busy | A live earlier direct child may own the inherited description. Never wait indefinitely, unlink, replace, steal or explicitly unlock the marker |
| Machine lease | Existing caller-configured `--lease-wait`, default 30 seconds total across turnstile and lease; contention exit 75 | One shared admission deadline, distinct from the zero-wait target marker |
| Capture after direct exit | 250 milliseconds for both output EOFs; then capture failure and kept state | Descendants can retain a pipe indefinitely under D41. Closing our pipe descriptors is allowed; their EOF is not proof that the direct child exited |
| Poll/service interval | Nominal 10 milliseconds; at most 64 KiB per pipe per service batch | Continuous output cannot starve signal observation, escalation or reaping; capture retains only its requested stdout budget and 16 KiB stderr |
| Target reset/deletion, cold removal at start/end/interrupt | No supported production operation yet: native D45 admission refuses before mutation | Native F2 is incomplete. No elapsed-time promise or destructive fallback is invented. The missing namespace primitive/assumption needs a product decision before native integration |
| Publication and filesystem synchronization | Failure-defined operations, no hard syscall wall bound; stop at the first error, keep marker and target, no fallback/retry | The pinned safe filesystem APIs cannot preempt a blocking open/stat/read/write/rename/unlink/sync. This also applies to validation during the signal/reap loop. Review must assess this disclosed limitation; a timer outside a syscall is not a cancellation mechanism |

The grace and reap values are distinct: nominal command teardown is at most five
seconds after observation **only while the OS calls and scheduler progress**,
plus finite poll/service overhead. This is not a hard wall limit for spawn,
filesystem calls, `kill`, or `try_wait`; normal build duration has no new timeout.
No lease release or completed status is claimed for a stuck kernel operation.
After the reap allowance, the Runner retains its handle until destruction; Drop
attempts KILL without resetting the allowance. If still unreaped, dropping std's
Child does not magically reap it: the inherited marker/kept state is the residual
protection, and teardown is explicitly incomplete. No target cleanup is attempted.

Mechanism: existing workspace Tokio 1.53.3, only `rt`, `signal`, `time`; existing
nix 0.31.3 additionally `signal`, `process` (`fs`, `dir`, `user` retained). Tokio's
safe Unix signal streams replace handwritten signal registration; a current-thread
reactor is driven by the synchronous command owner. nix sends a positive PID signal
only while that exclusive Child is unreaped; no process group is created or
signalled, no saved PID is signalled after reaping. D41 escaped-descendant risk is
unchanged. Capture uses safe nonblocking descriptor reads; no collector thread or
unbounded pipe join exists. A failed capture or closed output cannot gate teardown
logging. Lease exec also uses this owner and renders only after teardown, ignoring
stderr write errors.

ARCHITECTURE 4.8 check: both are existing maintained workspace libraries, with no
new version/resolution; cached pinned manifests declare Tokio MIT/MSRV 1.71 and
nix MIT/MSRV 1.69, compatible with pinned Rust 1.99.1 and `deny.toml`. Upstream
[Tokio](https://tokio.rs/) and its [signal API](https://docs.rs/tokio/latest/tokio/signal/unix/fn.signal.html)
document Unix listeners and their process-global lifetime; pinned local source
confirms that behavior for 1.53.3. [nix's pinned API](https://docs.rs/nix/0.31.3/nix/sys/signal/fn.kill.html)
documents positive-PID signaling. Tokio listeners coalesce signals and do not
restore default dispositions on Drop. This is a standalone CLI lifecycle, not an
embeddable service component. No unsafe code, new library, version/toolchain bump,
macros, multi-thread runtime, process or sync Tokio feature is added. The lockfile
adds only xtask's existing Tokio dependency edge.

If another actor removes or moves the marker, keeping state means retaining our
owned descriptor through teardown, withholding publication and leaving the target;
it does not mean recreating the missing name or resetting beside a live child.
The separately disclosed manual-removal residual is unchanged.

The marker owns one shared sticky cancellation listener. The command Runner
uses that same listener and re-observes its signal number after a failed validation,
so a HUP delivered during a marker check is never translated into TERM. The
listener stays alive through publication and is checked at every state step. This does not make a state
syscall atomic with signal observation: cancellation during a syscall is observed
at the next check. Marker removal is the current candidate finish point; a signal
concurrent with that operation may arrive after it has completed. Native admission
remains disabled, so this is no proof of a supported native publication/interrupt
transaction. F5 must retain a run-wide listener from before admission through final
reporting and integrate these outcomes once the native boundary is decided.

The legacy `gate.sh` remains **unwired** to this lifecycle. Because F2 cannot own
native admission/deletion, F3 does not replace the wrapper yet. F5's mandatory
replacement must remove both unconditional cold deletion and EXIT/INT/TERM cleanup
traps; it must not wrap this owner in another deleting shell. Do not use the old
wrapper as evidence for the new lifecycle. No full folded gate, native recovery,
Linux qualification, timing acceptance or release clearance follows from F3 tests.
