# Tiered verification checks

status: approved
owner: orchestrator
date: 2026-11-14
plan: [../plans/2026-11-14-tiered-checks.md](../plans/2026-11-14-tiered-checks.md)
decision record: [ADR 0013](../decisions/0013-tiered-verification-gates.md) (accepted at revision 2); [ADR 0014](../decisions/0014-gate-target-recovery-and-selection-coupling.md) (accepted) is its partial successor: it supersedes named clauses of decisions 30 and 31 and qualifies decisions 14, 15, 17 and 21. The owner's relaxation of ADR 0014's cessation rule on 2026-11-15 is recorded in [ADR 0015](../decisions/0015-cooperative-gate-target-recovery.md), which is proposed: its decision 41 is the owner's and is binding, and the implementation contract derived from it (decisions 42–47) is proposed, pending review

## Owner D45 native mutation amendment (2026-11-15)

The owner answered: “make a note in your report and relax it for now. because we
are going to change the storage backend in the future”. This supersedes the earlier
D45 checked-inode/ancestry-pinning requirement and the historical native-unavailable
status below. Under the configured cooperative lease, native admission, creation,
reset, cleanup and publication use safe descriptor-relative no-follow operations,
checking effective owner, device, type, private state-file modes and constant target
boundaries, then deleting or renaming by name. Neither checked-inode atomicity nor
pinning a directory's ancestry is required. Existing execution and dependency
amendments remain in force; no unsafe code, new dependency or version change.

**Owner-accepted interim risk, not resolved protection:** a process racing the last
check and deletion/rename can redirect that operation to a different user-owned
file. Revisit this relaxation when the storage backend changes; this work does not
design that backend. The three native interleaving characterizations remain tests
of this limitation, not protection passes. D41/FS1 escaped-descendant risk and the
cargo-clean/manual-marker-removal residual are separate and remain disclosed.

The native repair supplies a lifecycle API; final tier/bootstrap/report integration
belongs to the integration owner. Recovery of an existing marker remains conditional
on RC18 command/platform/filesystem evidence; D45 relaxation does not qualify it.
Measured Mac/Linux acceptance, R4/R11 names/counts, final-head Linux, nextest
qualification and whole-gate reviews remain due. Historical incomplete statuses
below describe their dated slices, not a continuing D45 execution prohibition.

The native repair now provides `native_admission` returning an inspected
`NativeTarget`, `NativeTarget::begin` acquiring/protecting the marker and deciding
reuse/reset, and consuming `NativeUse::run` to own direct commands through reap,
source-at-finish capture and warm publication or cold removal/keep. The caller
must retain the target and lease and route all direct commands through the supplied
runner. The callback's judged result and the target's publication result are
separate; failed compilation can finish a use without passing its step. A failed
runner, interruption, lost lease/marker or failed publication retains the marker
and target and does not qualify. Every state step revalidates the configured lease.
Invalidation occurs after the decision and before destructive reset, so a failed
reset leaves no reusable old record. Removal is failure-defined with the existing
inspection budgets; filesystem syscalls have no hard time guarantee.

Native controls exercise creation, stale/corrupt-record reset, warm reuse, cold
removal/keep, link canaries, failed publication and inherited lock last-close.
Recovery I/O after an unfinished use is tested with an explicitly injected policy
observation. Production `begin` still refuses any pre-existing marker until the
complete RC18 command/platform/filesystem matrix is qualified. A cargo observation
and hermetic child controls do not qualify git, rustc, all static commands, Linux
or every filesystem. This is a recovery-evidence condition, not the superseded
D45 mutation-capability refusal. Whole-tier integration/acceptance remains separate.

## Normal development execution amendment (2026-11-15)

The owner chose normal development execution: ordinary pinned-toolchain cargo
build/test in an isolated worktree under the cooperative machine lease. Earlier
static-only, controller, execution-contract and per-slice launch-review holds are
historical and superseded as execution preconditions. Their findings remain
**owner-set-aside as preconditions**, not resolved or proven. No special runner or
launch clearance is required. One security and one reliability review examine the
complete folded gate before PR 1; implementation slices are built and tested as
they go. Product correctness, hermetic tests, nonzero counts and all acceptance
criteria remain required. D41/FS1 remains owner-accepted risk.

F1 has scoped retained evidence; F2's direct-command/publication primitives have
scoped evidence, but native D45 admission/reset/removal remains incomplete at this
amendment. The continuation uses the existing resolved nix version through safe
APIs, only necessary features, with unsafe_code=forbid and no dependency upgrades.
F2 native repair precedes F3 signals/teardown, F4/F5 integration, C integration
tests and the artifact checkpoint. No per-slice review or controller loop is a
dependency. Native RC18 recovery evidence is command/platform/filesystem scoped;
unsupported capability refuses explicitly.

No third legacy Mac baseline, push, PR, merge, live rule change, release, tag or
version bump is authorized in this implementation wave. Final Linux evidence,
folded comparison/names/counts, native results, timings and whole-gate reviews
remain due. AGENTS.md and live rules remain unchanged.

## F3 signal and teardown implementation (2026-11-15)

The proposed D47 bounds are now recorded as one set in
[ADR 0015, F3 implementation proposal](../decisions/0015-cooperative-gate-target-recovery.md#f3-implementation-proposal-2026-11-15):
2s forwarded-signal grace, 3s KILL/reap (not reset by Drop), zero marker-lock wait,
existing 30s default machine-lease wait, 250ms post-reap capture EOF drain and a
nominal 10ms poll interval. Blocking filesystem/spawn/process syscalls have no hard
elapsed-time guarantee. Publication is failure-defined; native deletion has no
supported operation while D45 admission is unavailable. These values can be used
in ordinary development tests now and require the final whole-gate reliability
and security assessment before PR 1, not per-slice approval.

F3 owns only unreaped directly started children; it never creates/signals a process
group. Real status retention, no-record/kept outcomes on interruption or lost
lease/marker, KILL escalation and finite capture drainage need live hermetic
controls, including closed-output HUP and a descendant-held pipe. A fixture's
self-expiry or EOF is not teardown evidence. D41 does not waive directly started
child handling. Existing inherited-holder-loss control is updated to keep its
command live and require its real SIGTERM status rather than race a release file
against loss detection. No existing test is removed.

Native F2 admission/reset/cold deletion remains incomplete; independent F3 controls
cannot establish integrated native lifecycle success. The legacy wrapper is
unwired. F5 must replace its unconditional deletion and EXIT/INT/TERM traps, keep
a run-wide cancellation listener through reporting and integrate the candidate
publication cancellation checks once the native boundary is decided. No whole-gate,
Linux, timing, equivalence, PR or completion claim is made by this amendment.


## F4/F5 implementation status (2026-11-15)

Independent components now implement the sequential judged fold, a combined
ordered output pipe with a fixed capture budget, executable/name/count inventory,
nonzero-execution and witness checks, sentinel/shim checks from that same run,
metadata-only breadth-first real-home comparison, status/time tables, fixed
marker/kept diagnostics, redacted bounded evidence and read-only verification.
Private miniature workspaces exercise actual cargo build and test, including a
failed test, HOME write, shim call and zero-test suite. These controls are not a
workspace acceptance run, native target recovery, R4 comparison or R11 timing.

The production `full`/`pr` dispatcher now plans and reports a real refusal through
F2's unchanged native D45 guard. It does not execute a tier. Native admission,
reset, warm publication and cold deletion remain unsupported on both platforms;
`fast` remains PR2 work. The legacy wrapper's startup deletion and all target
cleanup traps are removed. While native reset is unavailable, bootstrap uses a
fresh private directory under `target/gate-xtask` on every invocation, retains it,
and rechecks its content identity before admission. This conservatively avoids a
stale judge but does not implement D27's reusable stamp or bounded retention.
This is an incomplete development branch, not an independently working PR1 gate.
The historical gate remains the reference at the base commit.

The suite explicitly builds workspace binaries before constructing the test map;
no path-executed `orch` is trusted from an older build. Rustup's Cargo bin directory
is included behind the shims so rustup cannot prepend it ahead of them; the witness
checks the resulting first entry. CLI coverage still excludes children with a
cleared/custom PATH; sentinel coverage excludes `.cargo` and `.rustup`. Source
capture refuses symlinks/submodules and rechecks set/inode/size/mtime around
hashing, but is not an atomic snapshot against concurrent replacement or a
same-size rewrite with restored timestamps. Judge capture likewise refuses links
and unsupported control characters in source names. The inherited locked marker
occupies stdin: R4/R11 must give ordinary and folded comparisons the same stdin or
diagnose that difference. No content identity or diagnostic proves cessation.

Remaining acceptance includes native D45 and RC18 on macOS/Linux, actual C
positive/negative/mutation CLI controls, run-wide lifecycle and reporting,
retention/contention sampling, R4 same-commit comparison with every difference
named, R11 executed names/counts/doc-tests and changed-body ledger, idle timing
R12, nextest R8 qualification, final-head Linux and whole-gate reviews. All
verification rows below remain pending. Execution authorization is distinct from
passing evidence; owner-set-aside launch findings stay on record.


## Source

Goal goal_7ZJmm9k, “Faster iteration: tiered checks instead of the full cold gate on every change”. The following numbered source passages are verbatim. The complete operational request is retained in the lead's private goal record; machine-specific scheduling details are omitted here.

> Tier definitions:
> The full cold gate stops being the unit of iteration.
>
> 1. Fast check (target: 1 to 5 minutes), for every commit and every fix round: fmt, clippy, and the tests of the changed crates plus the crates that depend on them, computed from `git diff` against a base and the cargo workspace graph. A change that touches only documentation runs no tests (build, lints and the documentation checks only). A change to Cargo.lock, rust-toolchain.toml, .cargo/, xtask/, .github/, or shared test support selects everything. The selection must print what it selected and why, and must fail closed (unknown file kind = everything).
> 2. Pull-request check (target: about 10 minutes, hard ceiling 20), once per pull request and the merge condition: ONE full suite run, the lint, advisories and crate-graph steps, warm target allowed. Test binaries run in parallel if the shake-out below succeeds.
> 3. Full cold gate (target: about 20 minutes), once per goal or release, not per pull request: cold target as today, with the empty-HOME check and the no-real-CLI check folded into the single suite run instead of two extra runs. Every protection the current script documents must still hold: nothing passes on a stale artifact; no test writes into a developer's home (the check must still run from outside the workspace so `.cargo/config.toml` [env] cannot hide a writer, and must still fail if zero tests ran); no test execs a real agent CLI; the real ~/.orchestry is untouched.
>
> Required shape:
> 1. `xtask/gate.sh` (or an xtask subcommand) offers the three tiers by name; the default invocation and its help say which tier is the merge condition and which is the release gate. The summary table keeps its per-step verdicts and adds per-step wall time.
> 2. Folding: when the suite runs under the sentinel HOME and the shims, test failures are judged in that run. Before switching, run the folded form once and report every test that passes today but fails folded (today the sentinel run discards test results); each is fixed or brought to the owner, none is silently skipped.
> 3. Parallel test binaries: evaluate cargo-nextest (process per test; no doc-tests, so doc-tests keep a separate step) against a simple split of the test binaries into N parallel cargo jobs. Measure both on the Mac and on Linux. Adopt one only if 20 consecutive runs are green on an otherwise idle machine and the wall time is at least halved; otherwise record the measurements and keep sequential binaries. Tests that need isolation get it explicitly (a serial group), not by slowing everything.
> 4. Slowest tests: for each binary above 30 seconds, say where the time goes (fixed sleeps, timeouts, process starts, real cargo builds inside the test). Remove fixed waits where a controlled clock or an event can replace them without weakening the assertion. No test is deleted or weakened to save time; a test that cannot be made faster stays and is named.
> 5. Change-based selection (tier 1) is a script with its own tests: for a table of changed-path sets it must select the expected crates, including the fail-closed cases.
> 6. CI: the pull-request workflow runs the tier 2 check once. A documentation-only pull request runs the documentation job only. Keep a full run on pushes to main.
> 7. Flaky tests are not managed by re-running: a test that fails intermittently is triaged and fixed or named, as in goal_zX0WXev. The tooling may record timings and failures per test to make that cheaper.
> 8. Team rules, changed by the owner's word at the approval gate and then written into AGENTS.md and the team's rule files: which tier a builder runs, which tier a reviewer may rely on, which tier gates a merge, one green CI execution per pull request instead of three, and the full cold gate once per goal. Until the owner says so, the existing rules stand.
> 9. Documentation: AGENTS.md, the developer section of the README or RUNBOOK, and the comments in the gate script say what each tier proves and what it does not.
>
> Acceptance:
> - A before-and-after timing table, measured, not estimated: each tier on the owner's Mac (idle machine) and the pull-request workflow on Linux CI; today's baseline is in "The problem".
> - Targets: tier 1 at most 5 minutes for a single-crate change and under 1 minute for a documentation-only change; tier 2 at most 20 minutes, 10 if parallel binaries are adopted; tier 3 at most 25 minutes.
> - Every protection listed in tier 3 has a negative test or a demonstrated failing case: a planted stale artifact, a test that writes into HOME, a test that execs a shimmed CLI, a suite that runs zero tests.
> - The selection script's table test passes, including the fail-closed rows.
> - The full suite passes in the new tier 3 form on macOS and in the new CI form on Linux, with the same number of tests executed as before (a count comparison, by name, in the report).
> - No test was removed; the report lists every test whose body changed and why.

Delivery source:
> tier=mid, stance=0.5, create PR. Small stacked pull requests against main, each leaving main working: 1 the folded gate with per-step timings; 2 the fast check with change-based selection and the documentation-only path; 3 CI workflow changes; 4 parallel binaries, only if the shake-out passes; 5 the slowest tests; 6 the rule and documentation changes after the owner's answers. The owner merges each PR; no release, tag or version bump. Approval gate first: present the open questions and the plan before building.

## Owner amendments to the quoted source

The Source above is kept verbatim as provenance. The owner answered the five open questions of the request and then approved the plan as a whole. The answers amend the Source. The owner's words are quoted exactly; the records are in the lead's private goal record. `<linux-host>` stands for the owner's native Linux worker; the host's name is not published.

| Question | Owner's words | What it decides |
|---|---|---|
| Q1, CI run count | “Q1 1.” | One green Linux execution at the final PR head is the merge rule. A failed run is kept and diagnosed, not rerun away. Independent review and the full gate once per goal or release remain. |
| Q2, native Linux worker | “1actually 2” | A suite run on `<linux-host>` may replace GitHub's Linux CI as the merge evidence. Chosen against the lead's recommendation. It does not by itself change a GitHub rule or permit a bypass. |
| Q3, parallel runner | “2” | cargo-nextest is the only candidate; the split into parallel cargo jobs is not evaluated. Doc-tests keep a separate step. The qualification bar of shape item 3 still applies, and retries stay disabled. |
| Q4, PR target | “1” | The PR check may use a warm target that is unique to its worktree, with the commit and tree recorded. The full gate stays cold. |
| Q5, machine lease | “1” | One cooperative host-wide lease for measured and gating runs. Reading, documentation and review continue. No process is terminated to free it. |
| Final plan | “1” | The plan is approved in full, including the removal of the required `test` check on `main` only, applied by the owner after PR 3 and the evidence procedure pass review. `check` and every `release/**` protection stay. Chosen against the monitor's recommendation. |
| Cessation rule of the gate's targets (2026-11-15) | “relax it” | A use of a gate-owned build directory counts as finished when the processes the gate itself started have exited and the gate holds the cooperative machine lease. Proof that escaped, detached or re-parented descendants have stopped is not required, on any platform. Chosen on the monitor's recommendation, against a blocking security finding, which stays on record as an accepted risk and not as resolved: a leftover process that the gate did not start can write into a build directory that a later run trusts. It waives no test, no review of the folded gate and no other decision in this table. Carried as ADR 0015 decision 41. |
| Unavailable host | “also the other thing - when one of [`<linux-host>`] or mac is down, you can just rely on the CI.” (host name replaced) | When `<linux-host>` or the Mac is unavailable, GitHub's Linux CI is the evidence for an ordinary pull request. A macOS check that did not run is reported as not run. This does not waive the per-platform qualification or the macOS cold gate for a goal or release; such a conflict returns to the owner. |

## Requirements

The commands and test paths below are the acceptance interface that the plan builds; they do not exist at the base commit. “Mac” means macOS. “Linux” means native Linux (GitHub CI or `<linux-host>`); an emulated container is labelled as such and does not count as native.

| Id | Requirement | Source | Priority | Acceptance check; platform |
|---|---|---|---|---|
| R1 | The gate shall expose `fast`, `pr` and `full` tiers by name, say which is the merge condition and which the release gate, and print per-step verdicts and wall time. | shape 1 | must | `./xtask/gate.sh --help`; `cargo test -p xtask --test gate_tiers` (plan vectors TV70–TV76); Mac + Linux |
| R2 | The full tier shall run the suite once, from outside the workspace, on a cold target, under a sentinel `HOME` and CLI recording shims, and shall fail when the suite fails or when no test executed. | tier 3; shape 2 | must | `./xtask/gate.sh full`; `cargo test -p xtask --test gate_protections` (TV1–TV6, TV12, TV17) and `--test gate_inventory` (TV7–TV11); Mac + Linux |
| R3 | The gate shall reject a planted stale artifact, a write into `HOME` and an exec of an agent CLI, keep the evidence of each, and protect the real global state. | tier 3; acceptance | must | `cargo test -p xtask --test gate_protections` (TV1–TV6, TV13–TV18) and `--test gate_bootstrap` (TV19), each negative case shown to fail for the stated reason; Mac + Linux |
| R4 | Before the folded run replaces the three runs, a comparison at one commit shall name every test whose result differs between the ordinary and the folded run; each is fixed or taken to the owner. | shape 2 | must | The two inventories and their difference, in the PR 1 report; Mac + Linux |
| R5 | Fast selection shall select the changed crates and their transitive dependents, select everything for shared inputs, unknown paths and errors, and print its reasons. | tier 1; shape 5 | must | `cargo test -p xtask --test gate_selection` (TV20–TV38); Mac + Linux |
| R6 | A documentation-only local fast check shall run build, lints and the documentation checks without tests; a documentation-only pull request shall run the documentation job only. | tier 1; shape 6 | must | `cargo test -p xtask --test gate_selection` (TV21, TV25, TV32, TV34); a real documentation-only pull request with the required checks satisfied (TV50); Mac + Linux CI |
| R7 | PR verification shall run one full suite plus lint, advisories and crate graph, keep a full run on pushes to `main`, and surface failures without rerunning them away. | tier 2; shape 6–7 | must | `./xtask/gate.sh pr`; real Linux CI runs of a code pull request and of `main` (TV51–TV53, TV55) |
| R8 | cargo-nextest alone shall be compared with the sequential baseline on Mac and native Linux. Adoption requires at most half the sequential wall time and 20 consecutive green runs per platform on an otherwise idle host, explicit serial groups, a separate doc-test step and no retries. | shape 3, amended by Q3 | must | Measured records, named inventories including doc-tests, and the 20-run records per platform. On a miss the sequential runner stays and the results go to the owner; nothing is substituted or waived silently |
| R9 | Each test binary above 30 seconds shall have its time attributed, and removable fixed waits shall be replaced without deleting tests, weakening assertions or raising timeouts. | shape 4; scope | must | Per-binary measurements before and after; a ledger of every changed test body with its reason; targeted regression tests; Mac + Linux |
| R10 | Team rules and developer documentation shall say what each tier proves and does not prove, and policy shall change only on the owner's word. | shape 8–9 | must | The owner's decisions as recorded above; commands in the documents run as written; an independent fact check of the final commit |
| R11 | The report shall give measured timings before and after and a comparison of executed tests by name and count on both platforms, and list every test whose body changed and why. | acceptance | must | Timing records and inventories that can be regenerated, checked independently against the logs and the diff; Mac + Linux CI |
| R12 | The fast check shall take at most 5 minutes for the named single-crate workload and under 1 minute for a documentation-only change; the PR check at most 20 minutes (10 if nextest is adopted); the full gate at most 25 minutes. | acceptance | must | `./xtask/gate.sh fast --base <base>` for the declared workloads, `./xtask/gate.sh pr`, `./xtask/gate.sh full`; wall times on an idle host; Mac + Linux PR CI. A miss goes to the owner and is never waived implicitly |
| R13 | Pull requests shall stay independently reviewable and owner-merged, with no release, tag or version bump. | delivery | must | Each PR's diff, status and CI evidence; no unrelated product, protocol or storage change |
| R14 | A run on `<linux-host>` shall count as merge evidence only when it records a clean tree at the exact PR head before and after, the pinned tools and configuration, a warm target unique to the worktree, the lease, complete logs, per-step exit status and the named test and doc-test inventory, and the PR links that record. Otherwise GitHub's PR run is the evidence. | Q1, Q2, unavailable-host answer | must | `cargo test -p xtask --test gate_evidence` (TV40–TV47); one real qualifying run, checked by a reviewer against the PR head; Linux |
| R15 | The rules change shall remove only the required `test` check, only on `main`, keeping `check` and every `release/**` protection including its `test` requirement. The owner applies it after PR 3 and the evidence procedure pass review. Until it is applied and verified, `check` and `test` both remain required. | final approval | must | The proposed configuration and its rollback, reviewed before use; the live rules read back after the owner's change (TV54); no bypass and no synthetic success |
| R16 | Measured and gating runs shall take a cooperative host-wide lease that survives interruption without a stale owner, shall record contention, and shall reject contaminated measurements; no other process is terminated. The full gate stays cold; PR targets are warm and unique to a worktree. | Q4, Q5 | must | `cargo test -p xtask --test gate_lease` (TV60–TV68); Mac + Linux |

## Non-goals

Deleting or weakening tests; raising timeouts to hide slowness; product changes beyond an injectable clock seam that already exists or is trivial; a remote build cache or a paid CI tier; Windows; changing what the crate-graph check enforces; any on-disk format or protocol. No release and no automatic merge.

## Constraints

The base is `main` at caefbadaf1f00c6fc9092acd1e01fcf26f894321. Every incident protection that `xtask/gate.sh` documents is kept, and a protection the script only describes in a comment is told apart from one it implements (the plan's incident ledger does this line by line). Existing rules apply until an explicit owner decision changes them. This document claims no measurement.

Decision reconciliation: [ADR 0014](../decisions/0014-gate-target-recovery-and-selection-coupling.md) is accepted, together with the lead's clarification of its decision 33 on the lifecycle of the warm trace. It supersedes named clauses of ADR 0013 decisions 30 and 31 and qualifies decisions 14, 15, 17 and 21; the rest of ADR 0013 stands. Its acceptance is of the design and clears no runtime hold. It changes no requirement, priority, source passage or acceptance check in this document. For R3 it means that a gate-owned target is published, removed, reset or reused only after the writers of its previous use are independently known to have ceased, also after a normal finish; the stale-artifact protection is otherwise not kept. An existing target counts as never used by a gate only on affirmative provenance; a target whose history is missing, unreadable, corrupt, incomplete or unknown is refused. For R5 it means that a selection containing `orch-cli` also selects `orch-infra`, and that a test which starts a binary by path runs only after that binary was built from the same tree; TV38 gains a case inside its existing number. For R16 it pins which process holds the lease. For R14 it means that a run whose cessation is unresolved never qualifies as evidence, in any tier. The requirements stay `must`. ADR 0014 does not supply a way to establish that cessation on a macOS host where all agents share one account. Until one is established and reviewed, the vectors under R3, R12 and R16 that expect a reused, reset or removed target cannot be met there. It does not supply the durable provenance either, on any host; until that exists and is reviewed, the deletion of a planted cold target (TV13) is unmet. Each of these is reported as unmet, never as a pass or a waiver.

Owner amendment of 2026-11-15 ([ADR 0015](../decisions/0015-cooperative-gate-target-recovery.md); the plan's eighth and ninth amendments). The paragraph above describes the contract as accepted on 2026-11-14 and is kept as written. The owner has since relaxed its cessation rule (the amendments table; ADR 0015 decision 41). That decision is binding. The contract that carries it out, ADR 0015 decisions 42–47, is proposed until the security and resilience reviewers have checked its exact commit and the lead has ruled. Under it, for R3: a gate-owned target is published, removed, reset or reused when its use is finished, which means that the processes the gate itself started have exited and the gate holds the configured cooperative lease; a run started without that lease is refused before it changes a target; a target left by a gate that died is reset only once the kernel lock of that use's marker is granted to a later run, and only where that recovery has been shown for the platform, the filesystem and the commands; a record that is missing, damaged or incomplete never permits a reuse and leads to a reset; and an existing target is no longer refused for want of a history, so the deletion of a planted cold target (TV13) no longer waits for a provenance that does not exist. R3 keeps its wording and its priority, with two residuals that are reported wherever R3 is reported. The first is the one the owner accepted: a process that the gate did not start itself can still write into a target, and nothing detects it. The second is a disclosed limit that the owner's decision does not cover: if an operator removes `target/`, with `cargo clean` or by hand, while a command that a killed gate had started is still running, the next run cannot see that command and builds beside it. The gate's refusal says not to do that; nothing prevents it. For R14: a run whose own use was not finished when it ended, or in which an operation on its target failed, never qualifies as evidence, in any tier. For R16: the target lifecycle requires the lease; the holder of decision 37 is unchanged. No requirement, priority, source passage or acceptance check in this document changes. No vector is passing or waived by this amendment: the gate is not built and nothing has run.

Security checklist:
- Default bind and exposure: no listener is added. CI keeps read-only permissions and does not persist checkout credentials.
- Bounds on input: changed paths, base refs and Cargo metadata are untrusted. Errors select everything. Paths are read NUL-delimited and passed as arguments, never through a shell.
- Files it writes: gate-owned targets inside the worktree, a gate-owned state directory beside them inside `target/` (the use marker and the warm record; ADR 0015 decision 43, proposed), disposable directories under the temporary directory, and one lease file at a fixed host path. Nothing outside gate-owned paths is deleted. Run logs are bounded in number. The state files are regular files of the gate's account with permissions for that account only; they are never opened through a symbolic link, the marker and every temporary file are created exclusively, no existing state file is truncated through its path, every read of them is bounded, and a link, a FIFO or another special file in their place is refused with nothing outside the state directory touched (ADR 0015 decision 45, proposed; the plan's case RC17 is the acceptance check, and the calls and limits are reviewed with the source).
- Secrets and environment passed to children: a sentinel home, shims in place of agent CLIs, explicit toolchain homes, no real agent CLI. The global-config redirect is removed from the suite's environment so it cannot mask a write into `HOME`. The commands the gate starts receive an open descriptor of the use marker's lock (ADR 0015 decision 43, proposed). That descriptor carries no secret, and it bears on safety: a later run is admitted only when no process holds it, so its lifetime and its release are part of the reviewed boundary (the plan's case RC18). The lease's descriptor is passed to no child.
- Untrusted text in output: evidence copies replace the repository root, the home directory and the temporary directory with placeholders, and record environment variable names from an allow-list, never values of credentials.

## Verification environment

The release engineer runs the host gates and Linux CI. The performance engineer owns the idle-host measurements. Independent reviewers use bounded disposable probes, not competing full gates. Comparable runner experiments need a native Linux host; an emulated container is labelled and cannot stand in for an idle native timing. `<linux-host>` may supply the PR evidence under R14, with GitHub's PR run as the fallback; measurements on real Linux CI are still required for R11 and R12. The live ruleset (id 25285267, covering the default branch and `release/**`) requires `check` and `test`; R15 describes the approved change and who applies it. The existing suite needs tmux. No user interface changes.

## Approval

The owner's answers to Q1–Q5 and the final approval are in the amendments table. Scoped implementation is authorized. Applying the rule change and merging each pull request remain the owner's actions. A measured timing or qualification miss returns to the owner.

## Verification

Filled in by the PR that completes the work. All rows are pending: no tier is implemented, and no acceptance check of a requirement has run. Controls of the test files' own helpers have run (plan section 12); they show that those helpers behave and are not a result for any row.

| Id | Result (`pass` / `env-limited` / `waived by owner`) | Commit the check ran at | Platform |
|---|---|---|---|
| R1 | | | |
| R2 | | | |
| R3 | | | |
| R4 | | | |
| R5 | | | |
| R6 | | | |
| R7 | | | |
| R8 | | | |
| R9 | | | |
| R10 | | | |
| R11 | | | |
| R12 | | | |
| R13 | | | |
| R14 | | | |
| R15 | | | |
| R16 | | | |

Defects found outside the requirements:
- `scratch_outlives_late_spawn_and_drop` in `crates/orch-daemon/tests/daemon_harness_hermetic.rs` was reported to fail intermittently with “conflict: diagnostic cancelled”. The root cause is unknown. The original failure is kept and is under investigation; it is not rerun away.
