# 0013. Tiered verification gates with one folded suite run

- Status: accepted (revision 2, the design baseline; not rewritten); named clauses of decisions 30 and 31 superseded by [0014](0014-gate-target-recovery-and-selection-coupling.md) (accepted 2026-11-14), which also qualifies decisions 14, 15, 17 and 21 without replacing a word of them. Only the clauses listed in 0014 under "What this changes in ADR 0013" are affected; every other decision, consequence and alternative of this record stands. 0014 was amended by its revision 3 on 2026-11-15: the owner relaxed the cessation rule (binding), and the implementation contract derived from it (0014 decisions 42–47) is proposed, pending review. The supersession and the qualifications named here continue as 0014 now states them; no word of this record is replaced
- Date: 2026-11-14
- Revision: 2 (reconciled with the design challenge)

Changes in revision 2. Decisions 1–24 keep their numbers; an amended one carries a pointer.

- New decisions 25–31: an ambient-configuration run kept in CI (25); inherited `ORCH_*` variables refused
  unless they are session variables (26, amends 8); the gate binary built from a content-checked directory
  and identified in the evidence (27, amends 3 and 17); `real-home` order and partial coverage (28, amends
  13); the stated limit of `no-real-cli` (29); the warm record written around builds (30, amends 15); package
  couplings in selection (31, amends 19).
- Alternative A2 gets a standing reopen trigger. New alternatives A19–A22.
- Consequences updated to match.

## Context

The owner asked for shorter iteration ([spec](../specs/2026-11-14-tiered-checks.md),
[plan](../plans/2026-11-14-tiered-checks.md)). `xtask/gate.sh` runs the whole suite three times: once plainly,
once under an empty sentinel `HOME`, once with the agent CLIs replaced by recording shims. The second and
third runs discard the suite's own result. Every change pays for all three.

Facts read from the code at the base commit, which the decisions below rest on:

- The sentinel and shim runs send the suite's output to `/dev/null` and ignore its exit status. Nothing counts
  the tests that ran. The comment about "zero tests" describes an incident, not a check.
- `real-home` reports "left alone" when `~/.orchestry` already existed. It compares nothing in that case.
- `advisories` passes when `cargo-deny` is absent.
- `.cargo/config.toml` `[env]` sets `ORCHESTRY_GLOBAL_CONFIG_PATH` for every process cargo starts from inside
  the workspace, `cargo run` included. A gate started through `cargo run` therefore inherits the redirect that
  the sentinel run must not have.
- Production code resolves the home directory from `HOME` and falls back to the account database when `HOME` is
  unset. A child started with a cleared environment writes to the real home, whatever `HOME` the gate set.
- `xtask` is a binary-only crate. Its integration tests can reach it only through the built binary.
- `std::fs::File::lock`, `lock_shared` and `try_lock` exist on the pinned toolchain (checked with a throwaway
  program). The lease needs no new dependency and no `unsafe`.

The owner's answers fix five points: one green Linux execution at the final PR head; a run on the owner's
native Linux worker (`<linux-host>`) may be that execution; cargo-nextest is the only parallel-runner
candidate; the PR check uses a warm target that is unique to its worktree; measured and gating runs take a
cooperative host-wide lease.

## Decision

Decisions are numbered so that a challenge can name one. "The gate" means `./xtask/gate.sh`, which starts the
Rust implementation in `xtask`.

### Tiers and entry point

1. **Three tiers, one step table.** `fast`, `pr` and `full` are rows of one table in code that lists each
   tier's steps, target directory and lease mode. There is no second implementation per tier.

   | Tier | Purpose | Target | Tests | Lease | Evidence it can be |
   |---|---|---|---|---|---|
   | `fast` | every commit and fix round | warm, gate-owned | selected packages, or none for documentation | shared | none; a builder's own signal |
   | `pr` | once per pull request | warm, gate-owned | whole workspace, once | exclusive | the merge condition |
   | `full` | once per goal or release | cold | whole workspace, once | exclusive | the goal and release gate |

2. **`./xtask/gate.sh` stays the entry point and the default tier stays `full`.** A bare invocation has always
   meant the release gate. Changing the default to a cheaper tier would turn an old habit into a weaker check
   without telling anyone. The default does not change in a later PR either. `--help` names `pr` as the merge
   condition and `full` as the goal and release gate. `--plan` prints the tier, its steps, its target and its
   lease mode, and runs nothing.

3. **The shell script only bootstraps.** It changes to the repository root, sets `CARGO_TARGET_DIR` to this
   worktree's own `target/` for building `xtask`, and runs `xtask gate` with the caller's arguments. Tier
   logic, help and parsing live in Rust (AGENTS.md: scripts that run again are `xtask` subcommands). The
   bootstrap ignores an inherited `CARGO_TARGET_DIR`, because a shared one is how a run once used another
   branch's binary. *Amended by decision 27: the build directory is the gate's own, and it is checked by
   content.*

4. **A step ends in one of three states: `pass`, `FAIL` or `ENV-LIMITED`.** `ENV-LIMITED` means a required
   tool is missing (for example `cargo-deny` in `pr` and `full`). A run with such a step is not green: its
   verdict is `ENV-LIMITED` and its exit status is non-zero. `fast` does not run advisories, so a fresh clone
   can still run a check without extra tools.

5. **Exit status.** `0`: every step passed. `1`: a step failed, or a preflight refused to start. `2`: usage
   error. `3`: nothing failed, but a step was `ENV-LIMITED`, or `--evidence` was asked for and the run does not
   qualify (decision 17). `75`: the lease was not obtained in the wait time, and nothing ran. `1` wins over `3`.

6. **A partial run never reads as a tier verdict.** `--part suite` and `--part static` exist so CI can keep its
   two required job names. The result of a partial run says `part:` and never `PASSED (tier …)`.

### The folded suite run

7. **One suite run, judged.** The suite runs once, from a working directory outside the workspace, under a
   disposable sentinel `HOME` and with the recording shims first on `PATH`. Its exit status and its test
   inventory decide the `test` step. `home-hygiene` and `no-real-cli` keep their rows; they read the sentinel
   directory and the shim log that this same run left behind.

8. **The suite's environment is built, not inherited.** The run sets `HOME`, `PATH`, `CARGO_HOME`,
   `RUSTUP_HOME`, `RUSTUP_TOOLCHAIN` (from `rust-toolchain.toml`) and `CARGO_TARGET_DIR` explicitly. It
   removes: every key that `.cargo/config.toml` `[env]` defines; every `ORCH_*`, `ORCHESTRY_*` and
   `XTASK_GATE_*` variable except the witness variable of decision 10; the four `XDG_*_HOME` variables; and
   `CODEX_HOME`. Removal of the `[env]` keys is a must: without it the port to Rust re-creates the incident in
   which the redirect hid a writer. Removal of `ORCH_*` keeps a test from reaching the real project daemon of
   the agent session that started the gate, and keeps an inherited opt-in variable (one of them rewrites
   golden fixtures) from changing what the run proves. *Amended by decision 26: only session variables are
   removed; any other inherited `ORCH_*` or `ORCHESTRY_*` variable refuses the run.*

9. **`test` passes only if four things hold.** The runner exits 0. The inventory has no failed test. At least
   one test executed. Every test executable that the build produced appears in the output with a completed
   result line. If the exit status and the inventory disagree, the step fails with "inconsistent inventory".
   The inventory is a sorted file of `kind`, `package`, `target`, `test`, `status`, kept with the run; it is
   what the before-and-after comparison by name uses.

10. **A witness proves the environment reached the tests.** One test in `xtask` does nothing unless the gate's
    witness variable names a directory; then it records the `HOME` it sees, whether the global-config
    redirect is set, the first `PATH` entry, and the names of any `ORCH_*` variables. After the suite the gate
    requires that record and requires it to match decision 8. Both historical failures of the sentinel check
    were of this kind: the check ran in an environment other than the one believed. The witness turns that
    belief into a checked fact on every run and with every runner. It describes the test process, not the
    children a test starts (decision 29).

11. **No verdict from a run that did not happen.** `home-hygiene` and `no-real-cli` pass only when the witness
    exists. If the suite did not start, they fail with "not evaluated". An empty sentinel after zero tests is
    not a clean result.

12. **The sentinel rule is unchanged: nothing may appear in it except `.cargo` and `.rustup`.** This keeps a
    known limit: a test that writes inside those two directories is not seen.

13. **`real-home` compares, without reading contents.** Before the first step the gate records the names,
    kinds, sizes and modification times of the entries under the real `~/.orchestry`, to a fixed depth and
    count. After the last step it compares. A difference fails the step and lists the paths. Absent before and
    absent after passes; created during the run fails. The check cannot tell a test's write from a write by
    another `orch` process on the host; it fails in both cases and says so. *Decision 28 fixes the order of
    the walk and what a comparison cut short by its bounds may claim.*

### Targets and stale artifacts

14. **`full` builds in `target/gate`, deleted before and after the run, as today.** The free-space refusal,
    `--keep`, and cleanup on failure or interruption stay.

15. **`fast` and `pr` share `target/gate-warm`, which the gate owns.** It is inside the worktree, never a
    symbolic link, and never taken from the environment. The gate records in it the worktree it belongs to,
    the toolchain, and a content identity for every tracked and untracked source file as of the last build. At
    the start of a warm run the gate resets the directory (deletes it and builds from nothing) when the record
    is missing or unreadable, when the worktree or toolchain differs, or when any file's content changed
    without a newer modification time. The result says `reused` or `reset: <reason>`. A warm run is still
    weaker than a cold one: it trusts cargo's fingerprints for everything the record does not cover. That is
    why `full` stays cold. *Decision 30 says when the record is written and what "newer" is measured
    against.*

16. **`--warm` remains a legacy option of `full`** with its current meaning: the developer's own target, and
    a result marked "not a gate". Whether to remove it is decided with the rule changes in the last PR.

### Evidence

17. **A run qualifies as evidence only when all of these hold:** it is a complete tier (no `--part`); it was
    started through `./xtask/gate.sh`; the tree was clean and `HEAD` was the same commit at start and end;
    every step passed; the exclusive lease was held at the default path; the toolchain is the pinned one. The
    result file states `qualifying: yes` or `no` with the reasons. `--evidence <dir>` copies the run's result,
    every step log, the inventory and a digest manifest to `<dir>`, with the repository root, the home
    directory and the temporary directory replaced by placeholders, and exits 3 if the run does not qualify.
    The host is identified by operating system, architecture and a label the operator passes, not by its
    name. `xtask gate evidence verify <dir> --commit <sha>` recomputes the digests, re-applies these conditions
    and compares the commit; it is what a reviewer runs before the owner merges. *Decision 27 adds the gate
    binary's source identity to these conditions.*

18. **Every run keeps all its step logs** in `target/gate-runs/<time>-<tier>-<commit>/`, with per-step wall
    time in the table and in the result. The ten most recent run directories are kept. `target/gate-result.txt`
    remains the result of the latest run and gains `tier:`, `part:` and `run-dir:` lines. A preflight refusal
    writes `REFUSED` and an interrupted run writes `INTERRUPTED`, so an old `PASSED` or a permanent
    `IN PROGRESS` cannot be mistaken for the current state.

### Selection

19. **The selector is a pure function of the changed paths and `cargo metadata`, and it fails closed.** Rules,
    first match wins for a path, and the widest result over all paths wins: an error of any kind selects
    everything; an empty change set selects everything (a wrong base is the likely cause); a path in the
    global list selects everything (`Cargo.lock`, any `Cargo.toml`, `rust-toolchain.toml`, `.cargo/`, `xtask/`,
    `.github/`, `.config/`, `clippy.toml`, `deny.toml`, `fixtures/`, `skills/`, `examples/`, `.gitignore`,
    `.gitattributes`); `docs/ARCHITECTURE.md` selects the `xtask` package, because `crate-graph` and its tests
    read it; a path in the documentation list (`docs/`, top-level `*.md`, `LICENSE`, `NOTICE`) selects no
    tests; a path under a workspace member selects that package and every member that depends on it through
    normal, build or development dependencies, transitively; anything else selects everything. Markdown
    inside `crates/` is code, because the crates embed it. Renames count under both names. The change set is
    the working tree against the merge base, plus untracked files. *Decision 31 adds package couplings.*

20. **The selector prints each path, its class, what it selected and why, before anything runs.** CI and the
    local `fast` tier use the same code.

### Lease

21. **The lease is a kernel file lock on one fixed host path.** `pr`, `full`, timing measurements and
    qualification runs take it exclusively. `fast` takes it shared, so fast checks run alongside each other
    and yield to a gating or measured run. The kernel releases the lock when the holder exits, however it
    exits, so there is no stale lease and nothing is ever signalled to obtain one. A holder record beside the
    lock is a diagnostic only. An exclusive waiter is not starved by a stream of shared holders: new shared
    holders queue behind it. A holder checks before each step that the lock path still names the file it
    locked; if a cleaner replaced the file, the run records the lease as lost and does not qualify. Waiting is bounded (default 30 minutes), then exit 75. The lease is cooperative:
    a plain `cargo` command does not take it. All agents on a host must run under one account; a lock file
    owned by another account is refused, not worked around.

22. **A lease does not prove an idle host, so the gate records contention.** It samples the load average and
    the build processes that are not its own descendants throughout the run. A gating verdict stands whatever
    the contention. A timing or qualification sample is valid only when no foreign build activity was seen
    and the data was available; an invalid sample is kept and reported, and is neither a pass nor a reason to
    discard a failure.

### CI

23. **`check` always runs and is the fail-closed anchor.** A first job runs the selector. `check` runs even
    when that job fails and then fails itself, so a broken selector cannot leave the required checks
    satisfied by skipped jobs. For a documentation-only pull request `check` runs the documentation checks
    and `test` is skipped by its job condition, which GitHub reports as satisfied. Otherwise `test` runs
    `./xtask/gate.sh pr --part suite`. A push to `main` never selects: it runs everything.

### Parallel runner

24. **cargo-nextest is adopted only if it qualifies:** at most half the sequential wall time, and 20
    consecutive green runs on each platform on an otherwise idle host. Retries are disabled on the command
    line, a run with no tests fails, tests that need isolation go in a named serial group, and doc-tests keep
    their own step. If it does not qualify, the sequential runner stays and the measurements go to the owner.
    It is an installed tool with a pinned version, not a crate in `Cargo.lock`.

### Added in revision 2

25. **One automated run stays in the ambient configuration.** From PR 3 on, every gate tier and CI's `test`
    job run the suite folded. So a CI job named `ambient` runs `cargo test --workspace --no-fail-fast` from
    the repository root on every push to `main`: inside the workspace and under cargo's `[env]`, which is
    the command CI's `test` job runs today and the configuration a developer gets from plain `cargo test`.
    It is not a required check, it does not run on pull requests, and no tier's verdict depends on it. Until
    PR 3 changes CI, the existing `test` job is that run, so `main` is never without one. A failure is kept
    and diagnosed like any other ([spec](../specs/2026-11-14-tiered-checks.md) R7). A test that fails there
    and passes folded is the standing trigger to reopen alternative A2. The job does not reproduce a
    developer's own `HOME` and `PATH`; a hosted runner's are nearly empty.

26. **Inherited `ORCH_*` and `ORCHESTRY_*` variables: session variables are removed, anything else refuses
    the run.** Decision 8 removed them all. That also removed a guard: the token-economy tests skip their
    old-reader half when `ORCH_T1_OLD_BIN` is unset, and `ORCH_T1_REQUIRE_OLD` exists to turn that skip into
    a failure. A gate started with both set would have dropped both, skipped, and passed. The gate now
    sorts what it inherits:
    - *Session and operator variables* are removed and their names printed. The list is fixed:
      `ORCH_AGENT_ID`, `ORCH_AGENT_NAME`, `ORCH_TASK_ID`, `ORCH_SOCKET`, `ORCH_PROJECT_ROOT`,
      `ORCH_TUI_CLIENT`, `ORCH_WEB_PASSWORD`, `ORCH_DAEMON_DETACHED_CHILD` (the names the daemon itself never
      hands to an agent), `ORCHESTRY_TASK_PROMPT`, `ORCHESTRY_DEBUG`, `ORCH_TUI_MOUSE`,
      `ORCH_REMOTE_ENV_PASSTHROUGH`, and every key of `.cargo/config.toml` `[env]`.
    - *The gate's own* `XTASK_GATE_*` variables are read by the gate and kept out of the suite's environment,
      except the witness variable.
    - *Any other* `ORCH_*` or `ORCHESTRY_*` variable is a preflight refusal: exit 1, `REFUSED`, the names
      listed (never the values), and nothing runs.

    This holds for every run that includes the suite. Unknown means refuse, as in the selector: a new opt-in
    variable is refused on the day it is added, without anyone maintaining a list. Opt-in tests are run
    directly with cargo, outside the gate; a gate proves the default configuration only.

27. **The gate binary is built from a directory that is checked by content, and the evidence names its
    source.** The binary that renders every verdict was the one artifact exempt from the stale-artifact
    rules: decision 3 built it in the developer's `target/` and trusted cargo's modification-time
    fingerprints, also for `full`. Now `gate.sh` builds `xtask` in `target/gate-xtask`, where nothing else
    builds. Before building it computes the *judge identity*: the content identity of every tracked and
    untracked, not ignored, file under `xtask/`, and of the root `Cargo.toml`, `Cargo.lock`,
    `rust-toolchain.toml` and `.cargo/config.toml`. If that differs from the identity stamped by the last
    completed build there, or no stamp exists, the directory is deleted and built from nothing. The stamp is
    removed before a build starts and written after it succeeds. `gate.sh` then starts the built binary
    directly and hands it the identity. The result and the evidence record it as `judge-source-id`; a run
    without one was not started through `gate.sh` and cannot qualify; `gate evidence verify --commit`
    recomputes the identity from that commit and rejects a difference. Price: `xtask` and its dependencies
    are rebuilt from nothing whenever those files change (not measured).

28. **`real-home` walks breadth-first and never calls a partial comparison "unchanged".** The walk of
    decision 13 visits a directory's entries in name order, level by level, and does not follow symbolic
    links. The top-level entries, `global.yml` among them, are therefore always compared, and which entries
    fall inside the bounds does not depend on the file system's order. When the depth or count bound cut the
    walk short and the compared part shows no difference, the step passes, its detail gives the number of
    entries compared and says `partial`, the result records `real-home-coverage: partial`, and the evidence
    carries that line. The word "unchanged" is used only for a complete comparison. Known limit: beyond the
    bounds, a rewrite of an existing file in place is not seen; a file created, deleted or renamed there is
    seen only when its parent directory is inside the bounds.

29. **`no-real-cli` sees an exec that resolves a name through the `PATH` the gate set, and nothing else.**
    A test that starts a child with a cleared environment and a `PATH` of its own is outside the step, and
    such a child that writes under a temporary `HOME` of its own is outside `home-hygiene` too. At the base
    commit 60 call sites in test code clear the environment; three set a `PATH` that includes host install
    directories, all three to find tmux. This limit is the three-run gate's as well; folding neither creates
    nor removes it. It is stated here, beside the limit of decision 12, so that the step is not read as
    covering descendants. The rule that tests never start a real agent CLI (AGENTS.md, Testing) remains the
    protection for that case.

30. **The warm record is written around a build, not only after a good one.** Before the first cargo
    command of a warm run builds in `target/gate-warm`, the gate captures the source identities and leaves a
    marker. When the last building command ends, with success, with a compile error or after an interrupt
    that the gate handled (once its children have exited), it writes the record with those identities and the
    end time, and removes the marker. The marker therefore survives only a gate that was killed. A run that
    finds it resets: `reset: previous build did not finish`. "Content changed without a newer modification time" means: a
    file's identity differs from the record and its modification time is not later than the recorded end
    time. Without this an interrupted build could leave artifacts newer than a record that still described
    the tree before it. The record's claims are about hosts where a worktree persists. On a hosted CI runner
    every job checks out at the same path with fresh modification times, so the worktree part of the record
    shows nothing there; it does not need to, because cargo then rebuilds every workspace crate.

31. **Selection also follows package couplings that the dependency graph cannot see.** The hand-kept
    coupling table of decision 19 gains rows of a second kind: a change in one package also selects another
    package whose tests run the first one's binary by path. One row today: a change in `orch-cli` also
    selects `orch-infra`, whose `concurrent_index` test starts the `orch` binary from the target directory.
    `fast` runs all selected packages in one cargo invocation, so that binary is built from the same tree
    before the test starts. A coupling missing from the table is caught by `pr`, not by `fast`.

## Consequences

- One suite run does the work of three. Test failures under the sentinel `HOME` and the shims, ignored until
  now, become failures of the gate. Each one found by the first folded run is fixed or taken to the owner
  ([spec](../specs/2026-11-14-tiered-checks.md) R4).
- The gate no longer runs the suite in the ambient configuration (inside the workspace, real `HOME`, real
  `PATH`). A test that passes only when isolated and fails for a developer is not seen by any tier. Two
  things cover it: the one-time comparison of the ordinary and folded runs, in both directions, and from
  PR 3 on the `ambient` job on every push to `main` (decision 25). That job reports after the merge, not
  before it: such a test can reach `main` and is found there.
- A gate started with an opt-in test variable set is refused. The token-economy matrix, a fixture blessing
  run and a longer stress soak are run directly, as their own scripts and documents already say.
- Each worktree that runs the gate holds one more small target directory, `target/gate-xtask`.
- A host whose global directory is larger or deeper than the bounds of `real-home` gets a passing step that
  says `partial`. A reviewer of evidence sees that line.
- `pr` and `full` need `cargo-deny`. A gate without it is `ENV-LIMITED`, not green. This replaces the earlier
  choice to skip the step so that a fresh clone could run the gate; `fast` now serves that case.
- `real-home` can fail because another `orch` process changed the real global directory during the run. That
  failure is reported, not hidden; if it happens often it returns to the owner.
- Each worktree that runs warm tiers holds a second target directory (several GB). Delete `target/gate-warm`
  when the work merges, with the worktree.
- Evidence from `<linux-host>` rests on the cooperation of whoever runs it. The digest manifest lets a reviewer
  check it against the PR head; it does not make it tamper-proof. The owner accepted this.
- After the owner applies the main-only rule change, GitHub no longer blocks a merge to `main` on a failing
  suite. Until then `check` and `test` both remain required.

## Alternatives rejected

- **A1. Keep three suite runs and only add tiers.** Leaves the two extra runs in place (about 35 of 54
  minutes in the owner's historical measurement) and keeps ignoring failures under the sentinel.
- **A2. Fold only the two discarded runs, keep the plain run (two runs).** Keeps the ambient configuration
  covered, at the price of one more suite run per gate (about 17 minutes in the same measurement). The one-time two-direction comparison answers the same question
  once, and the `ambient` job of decision 25 keeps answering it on every push to `main`. Reopen this when
  either one finds a test that depends on the ambient environment.
- **A3. Build the suite's environment from an allow-list.** Stricter than decision 8, but it changes every
  test's environment at once, so its fallout cannot be told apart from the fold's. Revisit if the removal
  list keeps growing.
- **A4. Use the developer's `target/` for warm tiers.** Saves disk. The gate would then delete or distrust a
  directory it does not own.
- **A5. Make `fast` the default tier after the transition.** See decision 2.
- **A6. A lock directory or PID file for the lease.** Needs stale-owner detection by process identity, and a
  crash leaves a lease behind. The kernel lock has neither problem.
- **A7. Take the exclusive lease for `fast` too.** The owner chose a lease for measured and gating runs only.
- **A8. A separate `docs` CI job with `check` and `test` both skipped.** A failed selector would then satisfy
  both required checks.
- **A9. Count tests only, without names.** A renamed or dropped test hides behind an equal count.
- **A10. Split the test binaries over parallel cargo jobs.** Dropped by the owner's answer.
- **A19. Run the ambient suite on the weekly schedule only.** One run a week instead of one per merge. A
  break then stays on `main` for up to a week and is not tied to the merge that caused it. It still meets
  the purpose of decision 25; choose it if runner time matters more than attribution.
- **A20. Refuse only a named list of opt-in test variables and remove every other one.** A new opt-in
  variable would be removed silently until someone remembered the list.
- **A21. Build the gate binary in the tier's own target and start it from a first-stage binary, or embed the
  source identity with a build script.** Two processes would share the lease, the signals and the cleanup;
  a build script is rerun by the same modification-time rule it is meant to check. The content stamp is a
  few lines in the bootstrap and needs nothing new.
- **A22. Report a partial `real-home` comparison as `ENV-LIMITED`.** A host with a deep global directory
  could then never produce a green gate, for a blind spot limited to rewrites in place below the bounds.
  Revisit if reviewers of evidence find `partial` common.
