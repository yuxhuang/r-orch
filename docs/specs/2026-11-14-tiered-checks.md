# Tiered verification checks

## PR2 integration — 2026-11-16 (acceptance pending)

The approved PR2 continuation starts at main `1603c67a3160901ce857f5877a03d227b980e6d9`.
It consumes the separately committed selector and SR-P1 fixture correction, then
implements fast and the local documentation-only path. CI, AGENTS/rules, versions,
dependencies and PR3–6 policy remain unchanged. Fast never supplies merge evidence.

- Fast uses shared machine admission in both Python bootstrap and Rust runner;
  the existing exclusive warm-target marker still excludes a second writer in
  one worktree. Queued exclusive admission blocks later shared requesters.
- Print selection/base/packages and quoted reasons. Keep global-first, fail-closed
  classification, reverse closure and D39 coupling. Empty changes select all.
- Run fmt/workspace clippy/crate graph first. Package/all paths explicitly build
  current selected targets before mapping/running tests (D40). Preserve sentinel,
  shims, witness, failed-suite status, independently nonzero selected inventory,
  warm-source checks and retained logs. When xtask is outside the selection, run
  its unfiltered witness target with its own map/inventory (all rows ok; named witness once) before the
  selected suite. The witness cannot rescue a zero-test selected suite.
- Docs-only runs build/lints/crate graph and no suite or witness tests, as approved.
  No advisories in fast, no new docs checker. Fast exports remain nonqualifying.
- TV72 now requires usable fast plans/runtime and invalid-option refusals. Existing
  full/pr/usage controls remain. TV61/62 and shared-fast RS1 use deterministic real
  requesters, including wrapper admission; no sleep is proof of ordering.

Implementation controls on macOS are distinct from acceptance. Native Linux D40,
shared controls and selector tables, isolated actual concurrent_index, independent
code/security/SRE review, native/pre-PR full checks/inventories and idle timing
remain outstanding. All historical option3 ACCEPTED NOT FIXED, FS1, interim D45,
manual cargo-clean/marker removal, M2 and other named limitations retain scope.
ADR 0015 remains PROPOSED. No acceptance requirement or risk is silently waived.

## NM1 prospective isolation, stage diagnostics and capture modes (2026-11-16)

The bounded C3/C1/DMS-1 correction follows independent review of the preceding
integration. S1 is independently closed on that delta; M2-message is closed
within its helper. Broad workspace M2/raw confirmation and native acceptance
remain open. Historical NM1 stage, holder and mechanism remain UNESTABLISHED
(turnstile is inferred only); original failed evidence is immutable. A measured
host mechanism includes transient descriptor retention by both posix_spawn and
fork. It supports a prospective mitigation, not historical attribution.

- C3 uses the existing isolated description-owner dispatch for
  `shared_lease_coexists_and_excludes_exclusive`, preserving its entire original
  assertion suffix and all zero waits. The child must still report exactly one
  passed test. Update only the existing two explanatory comments to include both
  spawn mechanisms; add no retry, wait or global serialization.
- C1 carries `admission-turnstile`, `lease-shared` or `lease-exclusive` through
  Contended and holder-diagnostic enrichment. Display adds `stage=<label>` while
  preserving the existing refusal wording, error class/downcast, exit 75,
  sanitization, deadlines, admission order and close-only lock semantics. Known
  turnstile and exclusive-lease holders distinguish both requested modes; a
  swapped-stage-label mutant must fail. No holder census is added.
- DMS-1 creates the shutdown capture directory with explicit 0700 permissions
  using the existing tempfile Builder API. The existing capture fixture asserts
  directory 0700 and regular, single-link 0600 streams under umask 022; removing
  creation permissions must fail. No global umask mutation, capture/reap wiring
  or deadline changes. Earlier reports claimed 0700 but measured 0755; private
  ancestors and 0600 files mean this discrepancy is not content-disclosure proof.
- Verify one changed-source xtask package, orch-infra lib plus spawn_gate,
  affected all-target check/clippy, fmt, crate-graph and explicit xtask build.
  Resolve the isolated exact name once with `--list --exact`; retain assertion
  suffix hashes. Use pre-run separate-channel JSON maps and original live logs
  from a private external Cargo cwd, absolute identities and unchanged inventory.
  Freeze clean source before workloads and preserve negative statuses/evidence.

Prospective interpretation: green is consistent with mitigation, not history;
isolated turnstile recurrence challenges the explanation; lease-stage recurrence
challenges fixture invariants. No unchanged retry is authorized. This correction
is scoped readiness only, subject to original security and SRE delta review.
ADR 0015 remains PROPOSED. Option3 ACCEPTED NOT FIXED, distinct FS1/D45/manual
removal risks, RC18 not prerequisite, historical C-N1 unknown/current mitigation,
R4/R11/idle/nextest/slow tests/PR2–6, both check/test and owner-only merges/rules
remain. Exporter, inventory parser, manifests, lockfiles and rules stay unchanged.

## Duplicate-member export and message-capture integration (2026-11-16)

This bounded amendment integrates the delivered exporter and M2-message leaves.
It supersedes the older storage/message exclusion below only for the shutdown
helper described here. Original failed evidence remains immutable. Implementation
readiness is distinct from independent S1/M2 review and native acceptance; ADR
0015 remains proposed.

- S1 inspection must visit every decoded object member, including duplicate
  plain names and Unicode-equal names, before any lossy map conversion. The
  existing serde public visitor retains an ordered vector of member pairs.
  Strings, keys, arrays and every member value reach recursive inspection;
  scalar leaves need no further decoding. Semantic inspection refusal propagates
  outside malformed-candidate handling, so a valid but unsafe fragment cannot be
  skipped as malformed. Benign duplicates remain exportable.
- Depth remains 128 and the shared per-input budget remains 128 MiB of charged
  candidate/decoded bytes and nodes, with candidate suffixes charged before parse.
  These are finite inspection budgets, not aggregate OS memory or wall bounds.
  Prepare and inspect every entry and structural string before creating the
  destination. Preserve short/non-UTF8 refusal, failed-suite status, nonqualification,
  public judge, manifest and source predicates; unknown secrets remain out of scope.
- Dependency wiring is only `serde.workspace = true` in xtask plus its matching
  existing serde lock edge. No package, library version, checksum or feature
  policy changes. The six added export regressions and two test-only helpers
  change no existing test body or assertion.
- M2-message captures both shutdown-test child streams to exclusive private
  regular files before spawn. Success emits nothing; failure retains complete
  labelled streams after actual reap, including unexpected-panic retention.
  Keep the external lock from before spawn through observed child exit, the
  readiness marker, SIGTERM, successful status, exactly-one-test summary and
  original 5-second readiness / 1-second shutdown deadlines. Capture/framing,
  real failure, live-child kill/reap and cleanup controls remain required.
- NM1 is unresolved: historical lock stage, holder and mechanism are
  UNESTABLISHED. The known-turnstile discriminator proves an alternative error
  mechanism, not historical attribution. Production lease code and the original
  shared-coexistence test remain byte-identical. A scoped green cannot diagnose
  or close the original Cargo101. NM3 closes invocation setup only. Historical
  C-N1 cause remains UNESTABLISHED; current isolation is mitigation only.

Combined verification runs the locked full xtask package once, orch-infra lib
and spawn_gate once, an explicit xtask build, affected all-target clippy, fmt and
crate graph. Collect maps before suites, raw JSON stdout separate from stderr,
from a private external Cargo cwd with canonical absolute manifest/target/Running
identities. Use unchanged product inventory on original live scoped logs; parsing
success never overrides Cargo failure. No full native acceptance run is authorized.
Retain cumulative107/21/later exact body/helper/minor ledgers and all original
failures. The actual union must enumerate added/changed bodies without removals,
weaker assertions, longer timeouts, suite serialization or retry-to-green.

Independent original S1 and M2/NM1 assessments consume the frozen combined source.
Other writers, M2-b/M2-c, n10–n13 and inherited minors remain disclosed, not closed.
Option3 is ACCEPTED NOT FIXED; FS1, interim D45 (revisit with storage changes) and
manual marker/cargo-clean removal stay distinct. RC18 is not a prerequisite.
R4 literal-base, complete R11, idle timing, nextest qualification or explicitly
approved fallback, slow tests and PR2–6 remain due. Both check/test and owner-only
merges/rules stand. No new risk acceptance or native/PR/goal clearance follows.

## Bounded native-test and export correction integration (2026-11-16)

This amendment carries the authorized MN1/LN1, MN3, C-N1-S, n7 and S1
corrections from the failed completed-head acceptance. Earlier results below
remain historical; their failed raw streams and incomplete verdicts are retained.
Scoped implementation checks do not replace changed-source native acceptance or
independent security and reliability assessment. ADR 0015 stays proposed.

- MN1/LN1: the shared test capture uses the existing gated diagnostic runtime.
  The spawn guard, its exception counts and all serving scheduler behavior stay
  unchanged. Capture retains both streams, actual status, cleanup/reap completion,
  exactly one passed test and the private-HOME audit.
- MN3: the hostile-environment test helper captures both child streams into
  exclusive private files. Its existing 60-second deadline, canaries and scenario
  assertions stay. It waits the owned child, requires exactly one passed and zero
  failed/ignored tests, and retains complete labelled failure diagnostics/files.
  Concurrent-output, zero-match and kill/reap/label controls remain required.
- C-N1-S: the twelve remaining paused Rig sibling scenarios receive the existing
  process-capture guard. Their timing, counts and event assertions stay exact.
  Historical C-N1 eligible-launch assertion/cause remains UNESTABLISHED; paused
  storage-retry dependency is measured, a cross-test holder is inferred, and
  isolation is current mitigation only. Feature-off tests share one private
  crate-root capture module under cfg(test); the feature-on facade is unchanged.
- n7: isolated_description_owner requires actual success and the final summary
  of exactly one passed, zero failed and zero ignored. A zero-match child exiting
  zero must be rejected. Existing lock assertions are unchanged.
- S1: after literal/escaped substitution, inspect JSON object, array and string
  fragments after arbitrary diagnostic text, recursively including decoded keys
  and values. A surviving known sensitive value refuses before export-directory
  creation. Depth remains 128; one per-input allowance of four maximum-log
  lengths (128 MiB charged bytes/nodes) covers decoded data and candidate suffixes
  charged before parsing, including malformed overlaps. Exhaustion refuses.
  Benign nested text remains exportable, and ordinary redacted exports, failed
  suite status, manifest/judge/source predicates and nonqualification stay intact.
  Private local records remain unredacted; unknown secrets remain outside scope.

MN2 changes the future ordinary capture recipe, not inventory parsing: use a
private external cwd, canonical absolute manifest and target paths, and separate
raw map stdout from Cargo stderr. Before the next full ordinary capture, a small
real Cargo capture must show raw absolute Running headers matching the original
JSON map and an actual unchanged-product inventory result. Keep unknown-path and
zero-executed-test negatives. Never transform old logs, recollect their maps or
broaden parsing to obtain acceptance; original ordinary inventory1 stays failed.

Additional static diagnostic writers in storage/message, index_cross_process and
failure-only child-output assertions remain outside this correction. No
workspace-wide M2/capture closure is claimed. Retain cumulative body/helper/minor
ledgers and enumerate every changed or added identity, with no removed tests,
weaker assertion or increased timeout. Combined verification covers the full
xtask package, infra spawn_gate, affected daemon feature-on/off targets, fmt,
workspace all-target clippy and crate graph. No full workspace suite or third
legacy baseline runs in this integration wave, and failures are never retried
unchanged to obtain green.

Option3 direct-writer/legacy overlap is ACCEPTED NOT FIXED. FS1, interim D45
(revisit with storage changes) and manual marker/cargo-clean removal remain
distinct; RC18 is not a prerequisite. R4 literal-base comparison, complete R11,
changed-source Mac folded/ordinary and native Linux PR acceptance, idle targets,
nextest qualification or explicitly approved fallback, slow-test work and PR2–6
remain due. Existing check/test and owner-only merges/rules stand. No PR or goal
acceptance follows from this amendment.

## Bounded Linux test completion (2026-11-16)

The preceding consolidated snapshot is incomplete: native Linux feature-off
units reported one failed wake case and one unfinished follow-up case before
owned-child cancellation, three daemon warnings escaped capture, and the xtask
stream contained 460 complete rows against 465 summary passes. Five unrecognised
rows are not five failed tests. The wake root cause is initially unestablished;
no flake or scheduler diagnosis is inferred from those observations.

A bounded native probe reproduced the in-flight stall at a context write.
A second diagnostic observed its index lock already free while the write stayed
pending; advancing only one millisecond let the write finish. This identifies
a paused-clock storage-retry dependency. Cross-test fork inheritance is the
source-based explanation for transient lock contention, not a directly observed
historical holder. The earlier eligible-launch assertion text was lost and its
exact cause remains unestablished. The two lock-sensitive wake scenarios receive
the existing private-process boundary; original timing and event assertions stay
unchanged. No diagnostic clock step is retained in their corrected bodies.

This continuation changes tests and test helpers only. Diagnose the wake rig's
clock, events and await ordering with bounded causal probes before correction.
Retain the 499/500 ms launch boundary, 60 s fallback distinction, attempt count,
in-flight tick, exactly one follow-up and no third tick. Use existing safe seams;
no longer wait, sleep, serialisation, ignored test or production scheduler change.
Attribute the legacy warning to its actual body before adding capture, and add
the existing capture to the Linux-only foreign-reservation body. Preserve child
status, cleanup and full diagnostics, including the private-HOME audit.

Route intentional fixture-retention diagnostics and unit-tier output through
libtest capture or a private test process. Retain unresolved and panicking scratch
with its complete reason and path. Never delete an unresolved target or suppress
production diagnostics. The inventory implementation and integration regression
file remain byte-identical, including all twelve framing cases. Original raw
negative/reverted controls must still name the actual failed outer test while
all complete outer rows reconcile; no transcript rewriting or parser expansion.

After correction freeze source and run one complete native xtask package on each
host, all thirteen affected CLI/daemon binaries, complete feature-off daemon
units, original-stream product-inventory audits, fmt, relevant all-target clippy,
crate graph and advisories. Arrange absolute executable-map/header paths before
Linux execution. Preserve the five prior correction controls, cumulative body,
helper and minor ledgers, exact commands/status/source identities and failures.
No full workspace acceptance suite runs here. A new defect is diagnosed and
reported without an unchanged package retry. Readiness requires every scoped
obligation; independent security and final native acceptance remain outstanding.

Option3 recovery overlap remains accepted, not fixed. FS1, interim D45 and manual
marker removal remain separate; RC18 is not a precondition. Existing check/test
rules, owner-only merges and all R4/R11/timing/nextest/PR2–6 obligations stand.

## Consolidated diagnostic and isolation completion (2026-11-16)

This authorized completion addresses the remaining narrow integration defects.
The previous integration snapshot and its failures remain historical evidence;
no scoped result supplies PR acceptance or closes independent security findings.

- Busy-lease diagnostics use a fallible stderr write and preserve exit 75 even
  when stderr is closed. The remaining lease last-close unit scenario receives
  the existing process ownership boundary, with all immediate inode and lock
  assertions retained and normal parallel test execution unchanged.
- Capture the daemon diagnostic subscriber unit scenario through the existing
  test-only capture mechanism, also when the testing feature is disabled.
  Production diagnostics and Cargo manifests remain unchanged.
- Both test capture helpers label every child diagnostic line before reporting
  failure. Child libtest headings remain diagnostic data; the unchanged product
  inventory must name the failed outer test and the suite must still fail.
  Retain complete child output, actual status, and all twelve existing inventory
  regression bodies; verify real red/fixed streams and reverted-render mutants.
- Audit the capture helper's private HOME before cleanup. Any entry or audit
  error fails with the child diagnostics retained. Planted HOME writers and AI
  blocker invocations must fail; no newly discovered writer is exempted.
- Record and compare the actual child.wait status exactly in the interrupted
  output-holder fixture. A nonzero status containing zero must be rejected.
  Only a retained-inode last-close lock grant authorizes subsequent target reuse;
  neither PID absence nor output EOF establishes cessation.

Verification requires exact-source complete xtask package checks on native Mac
and Linux, all thirteen affected CLI/daemon binaries and their outer streams,
feature-off daemon units, meaningful red/fixed/mutant controls, fmt, relevant
all-target clippy and crate graph checks. Use canonical private scratch and
normal workload umask under the existing turnstile then cooperative lease.
No full workspace suite runs in this completion. Final native measurements and
independent security disposition remain assigned acceptance work. The owner’s
option3 overlap is accepted, not fixed; FS1, interim D45 and manual-removal risks
remain separate. RC18 is not reinstated as a precondition. Existing rules and
owner-only merges stand; no release or PR authority is added.


## Whole-gate repair integration (2026-11-16)

This operative correction carries S1/M1/M2/L3 from the whole-gate assessment;
older implementation results below remain dated historical evidence. It grants
no PR clearance and changes no requirement or stage in PR2–6. Existing check/test
requirements and owner-only merges and rule application stand.

- M1: cancellation does not discard an earlier capture/reap failure. The real
  native finish path applies the same 250ms post-reap EOF deadline after a handled
  signal. An unresolved collector retains marker and target and cannot publish
  a record or remove a cold target. Clean cancellation before command creation
  starts no work. Native last-close fixtures isolate the marker-owning scenario
  in its own process while keeping original wait, exclusion and reset assertions;
  unrelated concurrent spawns can otherwise inherit a description before exec.
- S1: known nonempty sensitive values are redacted in literal, JSON-string and
  diagnostic-escaped output before export hashing. Recursively decoded JSON
  keys/values, including JSON text inside another log wrapper, must contain no
  surviving value. Shorter-than-four-byte or non-UTF8 sensitive values, decoding
  or nesting budget exhaustion, and unredactable manifest collisions explicitly
  refuse before export creation. No partial shareable bundle is left. Private
  local records remain unredacted; unknown secrets are outside this protection.
- m7: the witness checks the unchanged names-only removal record and every
  actually removed environment name. Required deliberately restored values are
  not marked removed. This detects Cargo-home/cwd-ancestor `[env]` reintroduction;
  arbitrary runners and separately reconfigured descendants remain unqualified.
  Repository-cwd static-step exposure and ambient Git identity inputs remain
  distinct disclosed conditions.
- M2: test-only process capture isolates permitted direct OS diagnostic writers
  before outer libtest framing, preserving the scenario, real exit/assertions and
  failure streams. The inventory parser stays fail-closed with all twelve
  ordinary-framing regression identities. The remaining uncaptured daemon
  diagnostics unit test is an implementation blocker, not an accepted rate.
- L3: after successful native Cargo reap and tooling-source recheck, the wrapper
  copies its owned regular executable through a no-follow descriptor into an
  exclusive private single-link executable, flushes/synchronizes it, rechecks
  source/locks/input identity, and publishes it before the completed stamp.
  Cargo's legitimate executable/deps aliases remain untouched. The judge's
  strict single-link, owner, path and matching-stamp predicates stay unchanged;
  arbitrary linked stamps/executables and direct unstamped runs do not qualify.
  Namespace publication retains the interim D45 checked-path/by-name race.

Implementation remains INCOMPLETE while busy-lease closed-stderr handling, the
remaining lease last-close fixture and the direct daemon diagnostic unit writer
need their existing owners' scope disposition. Scoped checks do not supply
changed-head full-workspace Mac/Linux acceptance. All failures remain retained;
no test is removed or weakened and no retry-to-green is authorized.

RC18's matrix is replaced **as a precondition** by the owner's option3 decision,
not universally proved. Any earlier instruction below to keep production recovery
Unsupported until RC18, or to give commands marker stdin, describes superseded
implementation history. Current commands get the extra descriptor and preserve
normal stdin; free same-inode marker recovery under the configured lease is
implemented. Finite native propagation/reset/last-close and mutation evidence
remains required. Option3 direct-writer/legacy-use overlap is accepted **not
fixed**; FS1 escaped/indirect descendants, D45 interim mutation races (revisit
with the storage backend), and manual marker/cargo-clean removal are separate.
Never remove an unresolved target to obtain a passing run.

R4 literal-base legacy comparison, actual R11 by-name/doc reconciliation,
changed-head green Linux and Mac acceptance, idle targets, nextest qualification
or approved sequential fallback, later slow-test work and PR2–6 remain due.


## Ordinary result framing repair (2026-11-15)

Recognize two bounded application-output frames in the original merged stream:
a numeric `orchestry X.Y.Z` version announcement occupying the status position,
followed by an exact, unindented terminal on the immediately next physical line;
and a terminal followed by the complete indented daemon stderr omission warning.
These are syntax rules, independent of test names, not arbitrary status prefixes
or a search for a later `ok`. Unknown status/chatter shapes still refuse.

Keep at most one pending named result, only outside nested helper intervals.
Its next line must be a complete terminal, without appended chatter; any blank,
other test, notice, helper, failure/header/summary boundary or EOF refuses.
Orphan standalone terminals refuse. Preserve FAILED and ignored statuses/reasons,
all existing complete grammars, summary/count/exit reconciliation, exact mapped
identities, duplicate/zero/filter/nested protections and failed-suite judgment.
Captured failure output remains data and cannot complete a pending result.
This parser cannot authenticate deliberately forged stdout; unsupported framing
fails conservatively. No input rewriting, schema, tool allowlist or test change
outside parser regression coverage is authorized.

Acceptance: regressions copied from both original raw shapes fail with the old
parser; focused positive/adversarial controls and prior inventory/evidence tests
pass. Replay both unmodified 7318-identity transcripts and historical 7278/1/7
using original executable maps; compare every named status and doc identity.
Preserve original ordinary helper1 and historical Cargo101/gate failure.
Replay is not final-head real-suite or whole-gate/platform acceptance.

## Real libtest inventory repair (2026-11-15)

Standard `test NAME has been running for over 60 seconds` is nonterminal:
require its named terminal result in the same completed group. A notice cannot
supply an executed test or relax count/status reconciliation.

Recognize nested successful single-test filtered helpers only with balanced
one-test announcements and summaries whose filtered complement matches the
outer announcement. Every excluded child observation must be a same-name,
same-status redundant row, occur between a paired child announcement/summary,
and leave exactly one outer row per identity. Match redundant observations in
chronological order; refuse if that conservative assignment cannot reconcile
the transcript. Arbitrary duplicates, conflicting statuses, missing child rows,
unbalanced helpers and unsupported nested shapes refuse. Child attribution may
be interchangeable; the resulting outer name/status inventory must be unique.
No test-name allowlist or arbitrary deduplication is introduced.

Rustdoc can emit multiple complete result groups beneath one mapped Doc-tests
header. Reconcile each group separately; retain the doc target until the next
executable header and reject duplicate identities across its groups. Incomplete
groups, ambiguous targets and process-exit disagreement still refuse. Consistent
failed input may parse, but Cargo 101 and the failed suite remain failed.

Acceptance is scoped: minimal red-first and adversarial inline fixtures, the
existing unit/inventory/evidence checks, and exact retained real log/map replay
must reconstruct 7278 passed / 1 failed / 7 ignored (docs 19/0/1). No real suite
rerun, schema change, whole-gate, Mac/Linux, R4/R11, nextest or PR acceptance is
implied. Preserve the failed source run and all newly observed failures.


## Owner-authorized inherited-descriptor recovery (2026-11-15)

The owner chose escalation option 3: “3. i'll accept the risk.” This section
controls conflicting recovery, standard-input and RC18 precondition statements
below, which are retained as historical design and evidence. It authorizes this
bounded implementation; it does not mark ADR 0015 accepted or clear whole-gate
review or runtime acceptance.

A direct command receives an extra descriptor duplicated from the marker's
locked open file description. Only that temporary duplicate has close-on-exec
cleared, using existing safe nix APIs; it is kept through spawn and closed in the
parent immediately afterward, including spawn failure. The original marker and
lease descriptors retain close-on-exec. Never explicitly unlock a shared marker.
The Runner preserves the command's normal configured stdin; the marker is never
substituted for it. No fixed descriptor number or environment token is required.

Under the configured exclusive cooperative lease, an unchanged same-inode marker
whose lock is granted permits native unfinished-use reset and rebuild (including
cold admission). Busy markers refuse before target mutation. Identity, structural,
invalid-record, source capture, publication, canary, actual exit/reap and lease
checks remain. Free lock means no holder of that open description, not cessation
of every writer or identification of a prior use. No prior-use state, format,
allowlist, custodian, fixture exception, dependency or unsafe code is introduced.

Three owner-accepted risks remain distinct: (1) FS1/D41 escaped or indirect
writers; (2) D45's interim checked-path/by-name mutation race, to revisit when the
storage backend changes; (3) direct-writer/legacy-use recovery overlap: a directly
started process, including one from the old stdin implementation, can close the
description and remain a live writer during reset. The third counterexample is
accepted, not fixed. Manual marker removal or cargo clean during unresolved use
is a separate disclosed, unprevented residual; operators must preserve such
targets. None of these is universal descendant protection.

RC18's command/platform/filesystem matrix is replaced **as a precondition** for
recovery. Its old evidence and failures stay historical. Real native verification
still requires descriptor propagation to Cargo/rustc/build scripts/tests/children
where observable, normal stdin, actual holder kill, live exclusion, explicit
release, independently observed exit/last-close, successful native reset/rebuild,
and post-use lock lifetime. The no-inheritance, shared unlock after handover, and
per-process-lock mutants must remain discriminating. Finite observations never
prove arbitrary descendants. The independent test owner corrects obsolete fd0
integration premises while preserving recovery positives and every other oracle.

Mac/Linux measurements, R4 same-commit ordinary/folded comparison, R11 identities,
counts/doc-tests/stdin effects, final-head green native Linux, nextest qualification
with sequential fallback, whole-gate security/reliability review and later PR
acceptance all remain due. Old 392/1 at 5569773 is historical, not changed-source
verification. No retry-to-green, third legacy baseline, push or PR is authorized.


status: approved
owner: orchestrator
date: 2026-11-14
plan: [../plans/2026-11-14-tiered-checks.md](../plans/2026-11-14-tiered-checks.md)
decision record: [ADR 0013](../decisions/0013-tiered-verification-gates.md) (accepted at revision 2); [ADR 0014](../decisions/0014-gate-target-recovery-and-selection-coupling.md) (accepted) is its partial successor: it supersedes named clauses of decisions 30 and 31 and qualifies decisions 14, 15, 17 and 21. The owner's relaxation of ADR 0014's cessation rule on 2026-11-15 is recorded in [ADR 0015](../decisions/0015-cooperative-gate-target-recovery.md), which is proposed: its decision 41 is the owner's and is binding, and the implementation contract derived from it (decisions 42–47) is proposed, pending review

## Integrated production defect repair (2026-11-15)

Historical implementation result; current capability and RC18 supersession are
defined by the owner-authorized recovery and whole-gate repair sections above.

Bounded native-recovery continuation starts at independent C head
`9c73b6e50d7b017e67bd65795c54ee483175e912`. Before any capability is enabled,
exercise real fixed commands through the production `NativeTarget`/`Runner`
holder, kill that holder, verify next-admission exclusion for the live direct
command, then observe actual exit/last-close and native reset. Cover command
lifetimes, exact tools/platform/filesystem, setuid-`ps` observation limits, and
three production-path discriminators: dropped inheritance, explicit shared-
description unlock with a resistant direct command, and per-process record-lock
replacement. Test-only seams may expose the production path but may not supply
recovery authority. Unknown combinations remain unsupported. If the mechanism
cannot meet the contract, retain the red positive and provide a concrete
counterexample and the minimum decision needed, rather than count unsupported
recovery as delivery. The original seven C files remain byte-identical.

The preceding repair started at independent C head
`608113814a64fdc12fd07141f14a3daf6efc4fce`.
The seven existing C test files stay byte-for-byte unchanged. This amendment adds
no contract relaxation and does not replace the original failed evidence.

| Repair | Required behavior and verification |
|---|---|
| RC18 unfinished use | Enumerate the actual fixed direct commands, tool versions, platform and filesystem; exercise actual marker inheritance, holder KILL, live-direct-process exclusion and last-close recovery, with discriminators. Enable only combinations supported by that evidence. Neither a platform allowlist, simulation policy, free lease, PID census nor EOF grants recovery. If qualification is incomplete, keep Unsupported and the real red recovery positive, and provide an explicit command qualification ledger. |
| Closed terminal | Fallible terminal output must not panic or bypass retained report/export/cleanup. Preserve handled TERM exit 143 and actual direct-child reap even when both terminal pipes are closed. Other terminal I/O errors remain failures, with their cause retained when possible. |
| Causal fault path | Preserve the causal structural error and offending `warm.record.tmp` path through lifecycle validation and retained/redacted diagnostics. Refuse the link without touching the outside canary. Keep the separate private regular-temporary publication-collision control. |
| Lease units | Retain public `--lease-wait <min>` default 30 minutes: approved plan section 1, CLI parsing and tier/bootstrap multiplication by 60 agree. The dated F3 proposed D47 table's seconds were stale; no numeric policy changes. |

Run the three exact C reproducers, qualification positive/negative controls, then
freeze a clean local SHA and run the full xtask package at that exact head plus
build, scoped clippy, fmt and crate-graph. Record identities/counts, commands,
statuses, source hashes and every changed/added test body. No full workspace or
benchmark acceptance is supplied by these checks. Whole-gate reviews, Mac/Linux
measurements, same-commit R4/R11, RC18 Linux, final-head Linux and nextest remain
due. D45's interim same-account check-to-mutation race (revisit when storage
changes), FS1 escaped descendants, and manual marker removal are distinct risks.


Repair outcome before freeze: terminal EPIPE and causal temporary-path corrections
pass the unchanged C reproducers. The added transient-link control also fails
against the predecessor runner, then passes restored. `gate_recovery.rs` exercises
19 real direct-command forms per filesystem on native macOS, on the worktree and
scratch filesystems: Git version/inventory/deleted/hash, rustc identity/version,
Cargo version/build/map/test/clippy/fmt/run-crate-graph, cargo-deny version/check,
ps, load/boot sysctl and notifyutil. The observed kernel lock is busy after the
primitive holder's KILL while the direct process is live, and granted after its
last close. Dropped-stdin and explicit-unlock discriminators grant the lock while
the command is live. These observations are **partial RC18**, not a successful
native recovery/reset: the holder is a primitive test fixture, individual
observations do not qualify complete command lifetimes, and direct stdin
inspection of macOS setuid `ps` is unavailable. The real C native-gate reset
positive remains red. Production `NativeTarget::begin` therefore still uses
Unsupported; no simulation policy or platform-name allowlist is promoted.
Linux, other tool sets/filesystems and native last-close reset remain unqualified.
The external handoff contains exact tool/filesystem identities and the incomplete
qualification ledger; none of these scoped controls is whole-workspace acceptance.


### Native-recovery continuation result (2026-11-15; incomplete)

The first private-tool view incorrectly canonicalized Cargo/rustc proxy links to
Rustup itself. The later tool-output audit exposed this; its native Rust-command
claims are withdrawn, with all raw results retained. The corrected fixture
resolves actual installed pinned backend binaries through a private resource-only
view, verifies native Cargo/rustc version output before the matrix, and requires
a new exact-head package run. This corrects the fixture, not the red recovery
positive; no capability was enabled by the invalid observations.

The explicit-unlock discriminator is applied only after the actual child has
inherited the locked description, acknowledged TERM resistance and remained
live beyond the grace. The earlier before-spawn unlock characterized an already
unlocked handover; it did not exercise release across existing sharers. Its raw
results remain partial and are superseded by this corrected control.

The continuation adds test-only access to the real `NativeTarget::begin`,
`NativeUse::run` and `Runner` start/drive path. Production recovery policy stays
unchanged. On the measured Mac/APFS devices, the 19 fixed real command forms
remain excluded after holder KILL while output backpressure keeps the direct
command live; real native admission refuses, the marker inode and artifact stay
unchanged, and exit/last-close are subsequently observed. The three production-
holder discriminators detect dropped stdin, explicit shared-description unlock
with a TERM-resistant direct child alive beyond the grace, and per-process
record-lock replacement (probed with the same record-lock API). Samples and
setuid-`ps` observation failures are retained; these finite workloads do not
prove arbitrary command inputs, future tool versions or unmeasured filesystems.
Successful native reset remains unmet and the unchanged C positive remains red.

A new concrete counterexample prevents enabling a capability based only on the
current tools/platform/filesystem: a previously admitted cargo-named tool closes
its marker stdin, ignores TERM, and stays live after the real production holder
is killed. Replacing that tool outside the source tree with native pinned Cargo
leaves the current tool combination native, the unchanged marker lock free, and
the earlier direct process live. The marker contains only unvalidated purpose/PID
diagnostics; it does not bind the prior use to a qualified tool set. Neither that
PID nor a current-tool check supplies the missing authority. The existing warm
record has already been invalidated. This is not the owner-accepted D45 namespace
race or FS1 escaped-descendant residual: the gate's own direct command is live.

No capability, diagnostic authority, format/protocol change or acceptance waiver
is introduced. The minimum new decision is whether to authorize authoritative
prior-use qualification/cutover state and launch enforcement (including refusal
of unknown prior uses), and permit the independent test owner to replace the
synthetic-cargo recovery fixture with an equally strong real-Cargo lifecycle.
Without that decision, the unchanged synthetic positive and a tool-specific
production qualification cannot both establish safe supported recovery. The
lead must adjudicate the counterexample before dependent implementation; the
existing independent verifier consumes these artifacts without another unchanged
probe loop. All other acceptance and risk disclosures above remain in force.

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

## Folded whole-run integration amendment (2026-11-15)

This amendment supersedes the dated F4/F5 unavailable-dispatch/bootstrap status
below; it does not rewrite historical evidence or change an acceptance row.
The native lifecycle is wired to cold `full`, warm `pr`, and the `--part`
dispatch. `full --warm` uses the developer target, is explicitly not a gate,
and never qualifies. `fast` and hosted-CI `--no-lease` reconciliation remain
later-PR work. No rules, toolchain, dependency versions or package versions are changed.
The release owner enables only the existing pinned nix hostname feature for its
safe read-only hostname API, so exports can redact this datum without starting
an unowned helper process. Export refuses if that privacy datum is unavailable.

D27 bootstrap uses the fixed `target/gate-xtask`, a completed judge-id stamp,
and content identities of tracked and nonignored untracked tooling inputs.
Missing/changed identities reset the bootstrap target; the stamp is removed
before every incremental build and published only after successful direct-child
reap and a matching second identity. The wrapper uses Python's existing standard
library and the configured turnstile/lease plus a persistent bootstrap kernel
lock. It does not remove a lock or delete a target from a signal/exit trap.
A killed bootstrap leaves no completed stamp; its Cargo child inherits the
bootstrap lock through stdin. The wrapper also accepts a live inherited machine
lease using the same bounded record/kernel-exclusive/challenge checks as Rust;
invalid, missing or stale capabilities fall back to normal acquisition. One
CLI-minute admission deadline covers turnstile plus lease, contention exits 75,
and bootstrap target-lock contention is an immediate refusal. No diagnostic PID
or record text alone grants authority. Accepted challenge streams explicitly use
blocking mode with the existing one-second read/write timeout: on macOS they can
inherit the listener's nonblocking mode and otherwise reject a valid client before
its bytes arrive. Delayed and fragmented challenges exercise the real holder;
invalid capabilities still grant nothing. Bootstrap and Rust source-name support differ:
the bootstrap encodes Git-quoted path bytes losslessly, while Rust judge capture
explicitly refuses unsupported control/non-UTF8 names. Such a run cannot qualify.

The whole-run producer reserves an IN PROGRESS report before admission, retains
step logs/inventory/environment names/tool versions and real command statuses,
and atomically publishes the final result. It keeps the ten latest completed,
validated run directories, preserving pending and unknown entries. The complete
marker denotes report completion, never a passed suite. A consistent inventory
helper may return zero with failed rows; the actual suite's exit and those rows
still fail the test step. Partial exports are nonqualifying and return 3 when
otherwise passing. A clean real full/pr run must establish all predicate fields,
the exact judge/commit/tree, pinned tools, default live lease and finished target.
A direct invocation without a matching stamp at the fixed bootstrap executable
path remains nonqualifying even if every check passes.

A run-wide sticky listener stays alive through reporting. After a handled signal,
only a finish-only path may run, and only after actual directly owned child reap;
it rechecks marker/lease and captures source identities before publication or
cold cleanup. The INTERRUPTED verdict and actual signal wait status remain.
No suite command resumes after cancellation. Unreaped children, output-collection
failure, lease/marker loss or failed publication keep the target and cannot
qualify. This narrows the native repair amendment's conservative all-interrupt
retention to the accepted D44/D45 finished-use rule. Tracked source deletions
are captured through Git's explicit deleted inventory and invalidate older sets;
a disappearing untracked source still fails capture.

Contention sampling occurs at start/end and every 15 seconds while the command
runner is driven. Probes own/reap their direct children, inherit the same marker
open description, and never acquire signalling authority from process metadata.
The sampler excludes the gate's process-parent closure, records maximum load,
and preserves unavailable query data. macOS reads the unprivileged thermal
pressure notification state; Linux discloses the unsupported portable query.
These observations establish no idle threshold or timing acceptance by themselves.
Probe statuses are retained separately from primary step indices; an unreaped
probe poisons the whole use, including handled-interrupt finish, and keeps the
target. Optional query failure is measurement data only after real probe reap.
The public judge content identity is preserved by export redaction; secret tokens,
session values and unknown gate-control values remain redacted.

D45 remains owner-accepted interim checked-path/by-name mutation risk, to revisit
when storage changes; it is not resolved. FS1 escaped descendants and manual
cargo-clean/marker removal remain separate residuals. Existing-marker production recovery uses the owner-authorized free same-inode
marker policy above; RC18 is replaced as a precondition, not universally proved. Historical launch findings remain owner-set-aside preconditions.
The unchanged seven C files are independent reproducers; fixture/contract
mismatches must be named for their owner, never silently edited here. Whole-gate
reviews, final-head Linux, Mac/Linux timings, R4/R11 names/counts and nextest's
qualification (or sequential fallback) remain due. No miniature is a workspace
acceptance run and no PR is authorized by this amendment.

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
existing 30-minute default machine-lease wait (the original F3 seconds label was stale), 250ms post-reap capture EOF drain and a
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

Historical implementation result; current capability and RC18 supersession are
defined by the owner-authorized recovery and whole-gate repair sections above.

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

Fixture isolation amendment (2026-11-15): the native qualification and
propagation observers must receive a fixture-owned `HOME` in the Rust command
before Python starts. Interpreter startup and child cache writes remain enabled
inside that private home. Both launch paths retain a startup HOME assertion and
record; fresh outer homes must remain recursively empty on the scratch and
worktree qualification runs and the propagation run. This repairs the fixture
boundary for R3 without exempting `Library`, removing sentinel entries or
changing the native lifecycle, recovery and accepted-risk assertions. Scoped
verification does not establish whole-gate or platform acceptance.

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

Filled in by the PR that completes the work. Requirement completion remains pending. The sequential folded tier and scoped controls are implemented; historical same-head Mac acceptance and failed Linux samples do not certify this corrected head. Controls of the test files' own helpers have run (plan section 12); they show that those helpers behave and are not a result for any row.

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

Fast has a known source-scanner gap: the orch-engine guard
`token_eco_preparation::dispatch_is_the_only_production_caller_of_adapter_execute_and_awaits_preparation_first`
reads every crate's source without a Cargo edge. The orch-infra guards
`spawn_gate` and `synthetic_spawn_domain_guard` also scan crates; D39 covers them
incidentally while every crate reaches orch-cli. This adds no coupling policy.
Fast is an iteration signal; the full PR check remains the merge check.
