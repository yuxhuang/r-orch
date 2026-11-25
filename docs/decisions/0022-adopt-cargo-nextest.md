# 0022. Adopt cargo-nextest as the gate's and the ambient run's test runner

- Status: accepted. Amends decision 24 and decision 25 of
  [0013](0013-tiered-verification-gates.md), and replaces no other word of that record: every other
  decision, consequence and alternative of 0013, and the qualifications carried through
  [0014](0014-gate-target-recovery-and-selection-coupling.md) and
  [0015](0015-cooperative-gate-target-recovery.md), stand.
- Date: 2026-11-24

## Context

Decision 24 of ADR 0013 set the condition for adopting `cargo-nextest`: at most half the sequential wall
time, and 20 consecutive green runs on each platform on an otherwise idle host. On 2026-11-17 the owner
**deferred** PR 4 after a dependency audit of the tool (version 0.9.152) found 23 advisories and 10 licence
items outside this repository's allow-list, and that deferral was written into `AGENTS.md`, `README.md`,
`docs/RUNBOOK.md` and the plan.

On **2026-11-24 the owner reversed that deferral and decided to adopt nextest**, and amended the bar:

- at most half the sequential wall time (unchanged), and
- **5** consecutive green runs on this Linux host, not 20 per platform.

Through 2.0 only Linux is developed, tested and gated (AGENTS.md rule 10), so the bar is Linux-only and no
macOS run is owed. The owner's amendment is the whole of the qualification this record claims; the remaining
15 runs of decision 24's series are **not** claimed, and this record does not grant them.

**What has been measured, and what has not.** Two quiet-host samples were taken before this
configuration existed, and they are the only timing this record cites. The `round1/` and `proto1/` names
below are the sample directories of the adoption's evidence set, which lives outside this repository and is
linked from the PR:

- `cargo test --workspace --no-fail-fast --locked`: **1014.7 s** (8,600 passed, 9 ignored, 244 result
  lines including doc-tests).
- a two-pass nextest prototype: **174.4 s** for the parallel pass and **221.4 s** for a `-j 1` serial pass,
  plus **4.2 s** for `cargo test --workspace --doc` — about **400 s**, roughly **2.5x**.

That prototype's serial pass covered seven orch-cli binaries, a broader set than the three this record
makes exclusive plus the one it puts in a group, so it is an **upper bound** on the final configuration's
exclusive work, not a measurement of it. **No timing has been measured for the configuration this record
adopts**, and the owner's decision was to adopt on those samples rather than re-measure. The 5 consecutive
green runs are a flakiness bar, not a timing one; any number for the final configuration would have to be
taken on a quiet host, and this one is shared.

**An estimate for the shipped two-pass configuration, from those samples.** It is an *estimate*: every
component below is measured, but their composition is arithmetic, not a run. Nothing here is a timing claim
for this configuration.

| Component | Estimate | Where the number comes from |
|---|---|---|
| parallel work of the `default` pass | ~**175 s** | the prototype's measured parallel pass, 174.4 s (`proto1/times.tsv`) |
| the exclusive tests inside that pass | ~**135 s** | 135.2 s, the summed per-test times of `daemon_admission`, `daemon_lifecycle_nospawn` and `daemon_skew_two_binaries` in the prototype's idle-host `-j 1` pass (`proto1/serial.log`); `threads-required` means nothing runs beside them, so the pass pays their sum |
| the `alone` pass | ~**20 s** | 20.34 s, `remote_peers`' 42 tests under libtest threads with one binary at a time (`round1/r1-cargotest.log`) — the condition the alone pass reproduces |
| doc-tests | **4.2 s** | measured (`round1/times.tsv`) |
| **total** | ~**335 s**, about **3x** | against the measured 1014.7 s baseline |

The `default` pass's parallel figure is a proxy, not that pass: the prototype's parallel pass excluded seven
binaries and included `remote_peers`, where the shipped one excludes only `remote_peers` and carries
`daemon_nodaemon_matrix` in a `max-threads = 1` group (21.5 s of summed per-test time on the idle host, which
overlaps other work rather than draining the run). The components of the estimate are also drawn from two
different samples. A real number needs a quiet host and a run of this configuration, and neither is claimed
here.

The owner also ruled that **there is one runner**: nextest replaces libtest for the suite outright, with no
runner option and no second path to fall back to. The speedup that matters is the one on the Linux
development host, because that is where development happens; an improvement on a GitHub-hosted runner is a
side effect and is not the evidence for the adoption.

The audit findings are **not resolved by this change**. Those 23 advisories and 10 licence items are in
nextest's own dependency tree, not in `Cargo.lock`: nextest is an installed tool, so `cargo deny` never saw
them and the `advisories` gate step is unaffected either way. No licence allowance and no advisory `ignore`
entry is authorized here, and the security review of those findings is separate from this record.

Two facts about the sequential runner shaped the design. First, `cargo test` runs one test binary at a time
and lets libtest thread the tests inside it; several test files depend on that, and say so in their own
comments. Second, nextest runs **every test in its own process**, which neutralises an in-process mutex
completely: three orch-cli binaries compare the host's whole process table against "no new `orch` process
exists", and the serialisation they relied on was cargo's, not their own.

## Decision

1. **nextest is the only runner of the suite step** in every gate tier (`fast`, `pr`, `full`) and of the
   `ambient` CI job. The gate runs one `cargo nextest run` per pass of the split (decision 4b) for the test
   binaries and one `cargo test --doc` for the doc-tests nextest does not run.

   Two commands in the repository still use libtest, deliberately and not as a fallback for the suite: the
   `fast` tier's `environment-witness` step runs `cargo test -p xtask --test gate_witness` (`suite.rs`), a
   single independently inventoried test that cannot turn an empty selected suite into a passing one; and
   `cargo xtask linux` runs `cargo test --workspace --no-fail-fast` inside its container, because its
   platform-quirk skips are libtest's `--skip` and that command is optional and never a gate.

2. **The version is pinned to 0.9.152 and the gate fails closed.** `cargo-nextest` absent from the suite
   `PATH`, or any other version, fails the `build` step with an install hint naming the exact version and
   the rollback. There is no silent fallback to another runner. A nextest run qualifies only with the
   pinned version.

3. **There is no runner option.** The suite step has one runner, so `--runner` is a usage error rather than
   a hidden second path, and the receipt and evidence verifiers have one parser. The rollback is to revert
   the change that adopted nextest, which is why it lands as one reviewable branch. Doc-tests stay on
   libtest only because nextest cannot run them.

4. **Isolation is expressed only in `.config/nextest.toml`,** and a guard test keeps that list honest. No
   test under `crates/**` is split, renamed, reordered or skipped for the runner's sake: the in-crate guards
   stay exactly as they are, inert under nextest but still serialising a plain `cargo test`. Three forms are
   used, and the file names the file and line of the evidence for each:
   - a `max-threads = 1` test group for a binary whose in-process mutex guarded a per-binary resource, so
     one of its tests runs at a time beside the rest of the workspace: orch-cli's
     `daemon_nodaemon_matrix`;
   - **a pass of its own** — the binary runs alone on the host, but its own tests run in parallel with each
     other: orch-cli's `remote_peers` (decision 4b);
   - `threads-required = "num-test-threads"` for a binary or a single test that observes a host-wide
     resource, so nothing runs beside it: orch-cli's `daemon_admission`, `daemon_lifecycle_nospawn` and
     `daemon_skew_two_binaries`, and one test of `daemon_nodaemon_matrix`.

4b. **The host's ephemeral port space is the third hazard, and the pass split is its remedy.** The first
   classification missed it entirely; qualification run 1 found it on 2026-11-24. `port()`
     (`crates/orch-cli/tests/remote_peers.rs:700`) and `free_loopback_port()`
     (`crates/orch-cli/tests/daemon_support/frontends.rs:472`) bind `127.0.0.1:0`, take the address and
     *release* the listener, and a peer then rebinds that same address across a restart. With one test
     binary at a time the address was almost never taken in between; with a process per test, ~128 of them
     binding ephemeral ports, another process wins the race and holds it past the retry deadline —
     `s23b_sigterm_run_watch_while_offline_keeps_the_remote_run` failed with "Address already in use" on
     `127.0.0.1:42369`. Those two are the only release-and-reuse helpers in the workspace: the other
     eighteen ephemeral binds hold their listener, so they reserve the port rather than releasing it.

   The remedy is not exclusivity but **reproducing the condition the tests were green under**. Under
   `cargo test` `remote_peers` ran as one binary at a time, its own 42 tests threaded inside it: they raced
   each other for ports, never the rest of the workspace. So the suite runs in **two passes** — one
   `cargo nextest run` for everything except `remote_peers`, then one for `remote_peers` alone at default
   parallelism, with nothing else on the host. The alternative, `threads-required` on the whole binary,
   serialises all 42: their per-test times in `round1/r1-nextest.log` sum to 257.9 s against a 19.8 s
   maximum, which **estimates** a ~570 s run, about 1.8x against the 1014.7 s libtest baseline — below the
   owner's 2x bar. *That figure is an estimate from summed per-test times, not a measurement, and no timing
   has been measured for the shipped configuration* (see Context). The two-pass split instead pays only
   `remote_peers`' own wall time, which is what libtest paid.

   The one test of `daemon_nodaemon_matrix` that reaches `free_loopback_port()` gets a **test-level**
   exclusive override rather than a place in the alone pass, so the rest of its binary keeps its group and
   still runs beside everything else. In the alone pass that test would compete for ports with the 42 tests
   running in parallel there — worse than libtest, where its binary's own mutex meant nothing ran beside it.
   Host-wide exclusivity reproduces that, and for one test it costs one test's duration.

   **The split cannot hide a test.** It is defined in exactly one place, the two profiles'
   complementary `default-filter`s in `.config/nextest.toml`; the gate, the ambient CI job and the guard all
   read that one definition, and the guard fails if any of them disagrees. The second profile is allowed to
   be nothing but `inherits = "default"` and its half of the split, and the guard names every table and key
   the file may have: with two profiles a whole-file check proves nothing, because `retries = 3` or a
   leak-timeout that forgives leaks would satisfy one from inside the second profile, and the ambient job
   forces neither flag on the command line. The alone pass's filter must also be a single `binary(...)`
   atom, because `not A & B` parses as `(not A) & B` — a conjunction there would make the first pass's
   `not <filter>` a different set from the complement and leave tests in neither pass. The gate takes its expected set
   from a single **unfiltered** listing (`cargo nextest list --ignore-default-filter`) and judges the
   **union** of the two passes' executed sets against it, so a test no pass ran has no result and a test two
   passes ran is a duplicate — both refuse. Both passes' exit statuses and every suite summary are checked,
   and one framed `steps/test.log` carries both passes (one record per pass, named by profile) so a receipt
   verifier still needs no extra file. A pass whose filter matched nothing (a tier that selects a subset of
   packages can leave one pass out of scope) is accepted only because that completeness check is judged
   against the unfiltered listing.

   Everything else runs in parallel. An over-wide exclusivity costs wall time and an under-wide one hides a
   flake, so each entry carries its evidence and nothing is listed because a grep matched it. In particular
   orch-cli's `daemon_lifecycle`, `fixture_daemon` and `stress_single_writer`, and orch-infra's
   `source_budget` and `process_endurance`, need **no** entry: every observation they make is scoped to a pid,
   a process group, a sandbox directory or a scene they own. `source_budget` measures its own child's peak
   RSS under its own address-space limit, and `process_endurance` picks pids above `pid_max` that cannot
   exist. Their wall-clock budgets (30 s per `source_budget` case) are load-sensitive, not
   isolation-sensitive: a loaded host can miss one, and that is diagnosed, never retried.

4a. **`xtask/tests/nextest_isolation.rs` enforces decision 4.** It scans `crates/**` and `xtask/**` for
   process-global locks, reads of the host's process table or `/proc`, and binds of an ephemeral port whose
   listener is not kept, and fails unless every hit is either named by an
   override in `.config/nextest.toml` or carried in its ledger with the reason it needs none — `Scoped`,
   `PerProcess` or `Production`. It fails in both directions: an unclassified hit fails, a ledger row whose
   pattern has gone fails as stale, and a configuration entry with no ledger row fails as unexplained. It
   also checks that every caller of an unscoped process-table helper has host-wide exclusivity, that every
   caller of a release-and-reuse port helper is in the alone pass or covered by a binary-wide or test-level
   exclusive entry (in both directions: a test-level entry in the configuration with no ledger row fails
   too, and a binary in the alone pass that has no such hazard fails as well, so the split cannot be used to
   quarantine an ordinary flake), that a test
   group really is `max-threads = 1`, and that the configuration still has `retries = 0`, no
   `terminate-after`, and a `default-filter` only as the two complementary halves of the pass split, each a
   single `binary(...)` atom — a third filter, a non-complementary pair, a conjunction and a missing half
   all fail, with negative controls. Because a second profile would let a setting escape every whole-file
   check, it also names every table and key the file may have: the two profiles, the first profile's
   overrides and the test groups, with the second profile allowed nothing but `inherits` and its half of the
   split. A `retries = 3`, a leak-timeout that forgives leaks, the exclusive overrides moved under the
   second profile and a third profile reachable through `inherits` are four more negative controls. It also
   checks that the gate's profile list, the ambient job's two steps and the configuration's profiles are the
   same passes in the same order, that the gate's listing is the unfiltered one, and that every flag the
   gate must not leave to the configuration is on the one command it builds per pass. A new test that needs
   isolation therefore cannot be merged without a deliberate entry.

5. **`retries` stays 0, and no timeout terminates a test.** A flaky test is fixed or named, never retried
   (AGENTS.md, "What gates a merge"), and no `slow-timeout.terminate-after` may turn a hang into a pass.

5a. **A leaked process is a named failure:** `leak-timeout = { period = "1s", result = "fail" }`. Under
   `cargo test` a process that a test left holding its output held the **gate's** own pipe, so the gate
   returned with its output unresolved and kept its target — that is how such a test used to redden a run.
   nextest captures each test's output, so the leak never reaches the gate and that signal would otherwise
   be lost; this setting reports it instead, and names the test that leaked. It is the opposite of masking:
   nextest terminates nothing for a leak, it only stops waiting for the pipe, so a hang cannot be turned
   into a pass. A full workspace run has zero leaks today (two measured runs), and the 1 s period is
   headroom over nextest's 100 ms default for a loaded host. The guard test refuses any weaker verdict.

6. **The inventory keeps every guarantee of the sequential one, from a different source.** nextest's own
   listing, taken once and **unfiltered**
   (`cargo nextest list --ignore-default-filter --message-format json`, not an experimental format), is the
   authoritative set of tests; it is kept in the build log beside the cargo executable map. The passes'
   event streams are judged against it as one **union**: every listed test needs exactly one terminal event
   with the status the listing implies, from exactly one pass, so a test no pass ran has no result and a
   test two passes ran is a duplicate. Every listed suite with at least one test needs an announcement whose
   count matches the listing and a completed summary, a suite that could not be queried (`status` other than
   `listed`) refuses the run, and a test the listing reports as filtered for any reason but `ignored`
   refuses it too — the listing is unfiltered, so a filter reaching it at all is a configuration the gate
   fails closed on. Unknown chatter, a malformed or truncated stream, an indented line, a duplicate result
   and a pass exit status other than 0, 100 or 4 are all errors; 4 is "this pass's filter matched nothing",
   legitimate for a tier whose package selection leaves a pass out of scope and never an excuse for a
   missing result, and all passes reporting it is the "no test executed" refusal. **Each pass's status is
   judged against its own stream**, not against the passes together: a pass exits 100 exactly when its own
   events report a failure and 4 exactly when it reported no suite at all, so one pass's failure cannot be
   answered by the other pass's exit and a pass that ran tests cannot be passed off as the empty one. That
   is what the frame's per-pass event counts are for (decision 8). Child chatter can cause a false failure,
   never an inferred pass.

   **Cargo keeps supplying an independent expected set.** The `build` step still runs
   `cargo test --no-run --message-format=json`, and its compiler artifacts must name exactly the same test
   binaries as nextest's listing: a binary Cargo built and nextest never listed, or the reverse, refuses the
   run. The runner does not get to supply both sides. The nextest inventory was also diffed once against the
   sequential one at the base commit, as evidence for this change; there is no ongoing dual-runner check,
   because there is no second runner.

   One exception is deliberate: a test the listing reports as **ignored** is not required to have a terminal
   event, because nextest 0.9.152 emits its skip event only sometimes (observed present for all five of
   orch-infra's ignored tests and absent for a two-test fixture binary). Requiring it would be an
   intermittent refusal.

   What the soundness rests on, stated precisely. An ignored test's status comes from the listing, which is
   the binary's own answer to `--list`; **a listed-ignored test with any result at all refuses the run**, and
   **every non-ignored test still needs exactly one terminal event**. Those two are what make a silently
   skipped test impossible. The suite summaries add two cross-checks of the run's own arithmetic — `passed`
   plus `failed` across a suite's groups must equal the number of tests that were not ignored, and
   `measured` must be zero — and a third, that every summary's `ignored` count equals the listing's. That
   last one is weaker than the others: nextest computes it from the same listing, so it detects a runner that
   contradicts itself, not one that agrees with a wrong listing. It is kept for that reason and claimed as no
   more.

   A run with no test at all still fails, on both halves: `--no-tests fail` makes nextest exit 4, which the
   parser reports as "no test executed", and the doc-test half is judged by the same sequential parser that
   refused it before.

   nextest's event names carry only `<package>::<binary-name>`, so a binary target named after its package's
   library cannot be told apart from it. The gate refuses such a listing rather than guessing; no target in
   this workspace collides.

6a. **Nothing is left to the environment.** Nothing filters `NEXTEST_*` out of the suite's environment, so
   every setting that could change a judgement is passed on the command line, where a flag beats the
   environment: the profile (`--profile default`, which an inherited `NEXTEST_PROFILE` would otherwise
   redirect to different isolation rules in the same file), `--retries 0`, `--no-tests fail`,
   `--success-output never`, `--status-level all`, `--no-fail-fast` and the message format and its version.

7. **The run's structured stream is read from stdout alone.** nextest's `libtest-json-plus` output, which
   nextest still calls experimental and gates behind `NEXTEST_EXPERIMENTAL_LIBTEST_JSON`, is acceptable
   only because of decision 2: the shape cannot change under a passing gate. Its operator log goes to
   stderr and is kept beside it, so a test's own output can never reach the stream the inventory is read
   from. `--message-format-version` is pinned as well.

8. **One `steps/test.log` still holds everything the inventory is rebuilt from.** The nextest log is framed:
   one leading JSON record per pass, named by its profile, carries that pass's exit status, the number of
   event lines it produced and its operator log, then one more carries the whole doc-test run with its own
   status, then the passes' raw event streams in pass order. The line counts are what put each event back
   with the pass that produced it, so a pass's status can be judged against its own stream; a count that
   does not describe the log — too few, too many, or absent — refuses on read-back, and a count that merely
   moves one pass's events to the other is refused by the status check instead. A receipt or evidence
   verifier therefore needs no extra files and no extra result fields; an unknown exit status refuses on
   read-back, and so does a log with a pass missing, renamed, reordered or added, because the frame is read
   against the gate's own profile list.

9. **`.config/nextest.toml` joins the judge's source set.** It is not a compile input, but it decides which
   tests may run beside which, so an edit to it during a run invalidates the run exactly as an edit to the
   judge's own sources does.

10. **CI installs the pinned runner from crates.io** (`cargo install cargo-nextest --locked --version
    0.9.152`) in the two jobs that run tests. That is the same provenance as the local install, with the
    tool's own lockfile, rather than a prebuilt binary or a third-party install action; the job gains no
    supply-chain surface the existing cargo build does not already have. The cost of that build on a
    GitHub-hosted runner is **not measured**; if it eats the suite's saving, a commit-pinned install action
    is the follow-up.

11. **Decision 25's ambient command is amended.** That decision pinned the `ambient` job to
    `cargo test --workspace --no-fail-fast` from the repository root. It is now the same two passes the gate
    runs — `cargo nextest run --profile default --workspace --no-fail-fast --no-tests fail` then the same
    with `--profile alone` — plus `cargo test --workspace --doc --no-fail-fast`, still from the repository
    root, still under cargo's `[env]`, still not a required check and still not run on pull requests.
    Everything else decision 25 says — why the job exists, that a failure there is kept and diagnosed, and
    that a test which fails there and passes folded reopens alternative A2 — stands.
    Every step after the first carries `if: ${{ !cancelled() }}`, which is this job's `--no-fail-fast`
    across steps: one pass failing must not hide the other pass's result or the doc-tests', which is the
    whole point of a job whose failures are kept and diagnosed. A cancellation still stops it, and the job
    still fails if any step did. `xtask/tests/ci_workflow.rs` pins the three new commands exactly, as it
    pinned the one before, and rejects a job that drops, reorders or merges a pass, or that drops a guard;
    `xtask/tests/nextest_isolation.rs` pins that the job's passes are the configuration's.

12. **nextest must apply Cargo's `[env]` table to the test processes it starts.** `.cargo/config.toml`
    redirects `ORCHESTRY_GLOBAL_CONFIG_PATH` and `ORCH_DATA_HOME` into the build directory so no
    cargo-driven test reaches the developer's real `~/.orchestry` (AGENTS.md, non-negotiable 5). A runner
    that builds through Cargo but starts the binaries itself has to carry that redirect, so
    `xtask/tests/ambient_env.rs` asserts it from inside a test process and fails the ambient job if it did
    not arrive. The gate is the one context where the variables are absent on purpose, and that case is
    recognised by the witness directory the gate sets, never by an unset variable alone.

## Consequences

- A developer needs the pinned tool: `cargo install cargo-nextest --locked --version 0.9.152`. Without it
  every tier refuses with that line, and no tier can run.
- `ambient` is now two commands instead of one. It is still the ordinary configuration, inside the
  workspace, under cargo's `[env]`, and still not a required check.
- The nine step names, the `qualifying` rules, the lease, the sentinel `HOME`, `no-real-cli`, `real-home`,
  the receipts and the time targets are unchanged. The time targets still rest on a measurement, not on a
  green run: passing proves nothing about timing.
- The private fixture suites in `xtask/` run the real runner: their sandboxes put the pinned
  `cargo-nextest` on the fixture's own `PATH`, check its version there, and write a minimal
  `.config/nextest.toml` into the fixture repository. So the planted negatives — a failing test, a write
  into the sentinel `HOME`, an exec of a shimmed CLI, a suite with no test at all — are judged on the
  runner that ships. A sandbox without the tool fails its setup with the install line rather than skipping.
- `cargo xtask linux` still runs the suite in its container with plain `cargo test`: its platform-quirk
  skips are libtest's `--skip`, the image would have to build the pinned tool, and the command is optional,
  never a gate, on a platform that is not qualified before 2.0. It is not offered as a fallback for the
  gate.
- Evidence records what ran: `tools.txt` carries `cargo-nextest`'s version line and the content hash of
  `.config/nextest.toml`, and `report::verify` requires both of a qualifying bundle. The hash is there for a
  reader; what *binds* the isolation rules to the commit is `judge-source-id`, because that file is one of
  the judge's source inputs (decision 9). The verifier deliberately does not compare the hash to the working
  tree: it runs against an arbitrary checkout, and doing so would reject a valid bundle.
- Every future test that observes a host-wide resource, or that relies on an in-process lock, needs a line
  in `.config/nextest.toml`. A test that quietly needs one will show as a flake, which is fixed or named,
  never retried.
- **The tool's own dependency tree is a second, unvetted one, and CI builds it.**
  `cargo install cargo-nextest --locked` compiles nextest's own lockfile — its build scripts and
  proc-macros included — inside the `test` and `ambient` jobs. `cargo deny` covers r-orch's `Cargo.lock`
  and nothing else, so that tree is unreviewed by this repository's supply-chain checks. It is accepted
  deliberately, bounded by what those jobs can reach: `contents: read`, `persist-credentials: false`, no
  secrets, and nothing from the build is shipped or linked into r-orch.
- **nextest links what the `check` job bans from r-orch.** The security review of 2026-11-24 found that
  nextest-runner 0.130.0 links `terminfo 0.9.0` (WTFPL) and `termwiz` non-optionally, through
  sapling-streampager — exactly the two crates the `check` job's `Cargo.lock` guard excludes from r-orch.
  That is accepted because nextest is an out-of-tree build tool: it is never linked into r-orch, never
  shipped with it, and its licences never reach a built binary, archive or package. The guard on r-orch's
  own lockfile is unchanged and still absolute.
- **Where the audit's 23 advisories stand.** From the same review, by reachability in this use: 5 are in
  dev-dependencies that are never built; 13 are behind the `nextest archive` and run-recording paths, which
  no gate tier or CI job invokes; 4 are unmaintained-crate notices with no advisory; and about 5 are
  in-process unsoundness with no path from untrusted input. None is reachable in the way this repository
  uses the tool. That is a reachability summary, not a resolution: the findings stand, no licence allowance
  or advisory `ignore` is authorized, and this record claims nothing about the audit beyond where its items
  sit.
- **Cancellation changed, and this needs an owner decision.** On a gate-only TERM, nextest cancels, signals
  each running test's process group and exits 0.207 s later (measured), well inside the gate's 2 s grace, and
  it closes its own capture pipes as it goes. So the gate's use **finishes** and publishes a target record,
  where `cargo test` had to be killed with its output unresolved, which kept the target. Two consequences
  follow: a later run trusts a target whose suite was cancelled, and a test child that ignores TERM survives
  — reparented, holding nextest's capture pipe rather than the gate's, so the gate's "descendant may hold
  output" check cannot see it, and with the marker removed by the finished use nothing records it at all.
  This is the one place where the adoption makes the standing accepted risk for processes the gate did not
  start (AGENTS.md "What no tier proves"; ADR 0015 decision 41) *easier* to reach rather than harder.
  `suite_cancellation_finishes_the_use_and_leaves_a_term_ignoring_test_child` pins the behaviour so it cannot
  change unnoticed. No fix is applied here: refusing to publish a record for a cancelled run would change
  the target lifecycle for every tier and step, which is the owner's call, not this record's.
- The gate's 2 s signal grace is **not** the limiting factor and is unchanged: nextest exits inside it, so
  the gate never reaches SIGKILL. Lengthening it would alter every step's interrupt behaviour for no gain.
- The 32 MiB capture budget per step now holds a framed log: nextest's event stream, its operator log and
  the whole doc-test run. A full workspace run produces a few megabytes, and exceeding the budget fails the
  step closed rather than truncating the inventory.
- **The first classification was incomplete, and the shape of the gap is worth recording.** It was derived
  from process-global locks and process-table reads, and it missed a host-wide resource that no lock or
  table read mentions: the ephemeral port space. Qualification run 1 found it. Two lessons are now in the
  guard rather than in anyone's memory — the detector covers released ephemeral ports, and the ledger must
  state the *hazard* rather than the lock — but the general point stands: a host-wide resource that a test
  reaches through a helper, not through a named global, is invisible to a source scan until someone names
  it. A failure here is diagnosed and classified, never retried.
- `remote_peers`' own pass costs the suite a second serial segment: nothing else runs while it does, so the
  suite pays its wall time end to end rather than overlapping it. That is what libtest paid for the same
  binary (20.3 s of threaded tests on a quiet host), which is why the alone pass was chosen over
  `threads-required` on the binary — that would have cost the 257.9 s sum of its tests instead. No timing is
  claimed for the configuration as it ships; the estimate is in Context.
- A test added to `remote_peers` lands in the alone pass automatically, because the split is by binary. That
  is the intended behaviour — the next test to reach `port()` is covered without anyone noticing it — but it
  also means a slow test added there is paid serially. The guard states the hazard, not a test list, so
  nothing warns about the cost.
- **Two tests are named as load-sensitive, under the rule that a flaky test is fixed or named.** Both are
  named rather than fixed because neither is this change's to fix: the second is under `crates/**`, which
  this change does not touch, and the first is a fixture of the gate's own publication boundary that this
  change does not alter. Naming is therefore the compliant action, not the convenient one, and a recurrence
  is a defect to fix rather than a result to re-run.

  `orch-daemon::daemon_prelaunch_failure::a_read_queued_during_a_failure_batch_is_served_before_the_batch_ends`
  failed once in the `pr` gate run of 2026-11-25, in the child the test re-execs
  (`crates/orch-daemon/src/testing/output.rs:124`), which panicked at
  `crates/orch-daemon/tests/support/mod.rs:146` with "connection closed before reply". The same run's own
  contention sampling recorded `foreign-build-activity: yes`, a one-minute load peak of **124.79** and up to
  5 foreign builds: another agent was running a bare `cargo test` outside the cooperative lease throughout.
  It is **not** an isolation gap — the re-exec'd child gets a private `HOME`, `TMPDIR`, global config and
  `PATH`, so it shares no host resource with another test, and the test is a parking/unparking rendezvous on
  a `TestClock`, the shape that breaks when the scheduler stalls one side. It passed in the base commit's
  nextest sample and in the libtest baseline. One failure is not a diagnosis of flakiness, which is exactly
  why it is named here: if it fails again, it is fixed, not retried.

- **A gate fixture is also load-sensitive, and is left as it is.**
  `xtask::gate::target::tests::f3_publication_boundary_observes_cancellation_after_command_owner_lifetime`
  failed once during this work, at a load average of 141 while another agent's run overlapped this host, and
  passed 13 of 13 other times. It is a timing-sensitive fixture of the gate's own publication boundary, not
  a product test, and nothing in this change touches it. It is named here so that a future failure is
  recognised as this known sensitivity and diagnosed rather than retried: under this repository's rules it
  is fixed or named, and this is the naming.
- The GitHub-runner speedup is **not claimed**: the measurement is of the Linux development host, which is
  where the owner wants the suite to be faster. CI also pays to build the tool, and the PR's own CI run is
  the only 4-vCPU observation there will be.
