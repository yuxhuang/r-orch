# 0013. Tiered verification gates with one folded suite run

- Status: proposed
- Date: 2026-11-14
- Revision: 1 (design v1, before the design challenge)

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
   branch's binary.

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
   golden fixtures) from changing what the run proves.

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
    belief into a checked fact on every run and with every runner.

11. **No verdict from a run that did not happen.** `home-hygiene` and `no-real-cli` pass only when the witness
    exists. If the suite did not start, they fail with "not evaluated". An empty sentinel after zero tests is
    not a clean result.

12. **The sentinel rule is unchanged: nothing may appear in it except `.cargo` and `.rustup`.** This keeps a
    known limit: a test that writes inside those two directories is not seen.

13. **`real-home` compares, without reading contents.** Before the first step the gate records the names,
    kinds, sizes and modification times of the entries under the real `~/.orchestry`, to a fixed depth and
    count. After the last step it compares. A difference fails the step and lists the paths. Absent before and
    absent after passes; created during the run fails. The check cannot tell a test's write from a write by
    another `orch` process on the host; it fails in both cases and says so.

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
    why `full` stays cold.

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
    and compares the commit; it is what a reviewer runs before the owner merges.

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
    the working tree against the merge base, plus untracked files.

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

## Consequences

- One suite run does the work of three. Test failures under the sentinel `HOME` and the shims, ignored until
  now, become failures of the gate. Each one found by the first folded run is fixed or taken to the owner
  ([spec](../specs/2026-11-14-tiered-checks.md) R4).
- The gate no longer runs the suite in the ambient configuration (inside the workspace, real `HOME`, real
  `PATH`). A test that passes only when isolated and fails for a developer is not seen by the gate. The
  one-time comparison of the ordinary and folded runs reports such tests in both directions.
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
  once. Reopen this if that comparison finds tests that depend on the ambient environment.
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
