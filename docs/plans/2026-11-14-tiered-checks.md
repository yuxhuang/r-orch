# Tiered verification checks — plan

status: active
owner: orchestrator
date: 2026-11-14
spec: [../specs/2026-11-14-tiered-checks.md](../specs/2026-11-14-tiered-checks.md)
decision record: [ADR 0013](../decisions/0013-tiered-verification-gates.md)
design revision: v2 (reconciled with the design challenge)

Changes from v1: ADR decisions D25–D31 and alternatives A19–A22; findings F14–F16; sections 3, 4, 5, 7 and 8 follow those decisions; new section 11 (the interface the builders share); new vectors TV17–TV19, TV38, TV47 and TV55, and a third case in TV15; the comparison procedure for R4 in the test plan; open items 2 and 5 updated, 7–11 added. No requirement, task id or existing vector number changed.

## Goal

A builder gets a verdict on a change in minutes (`fast`), a pull request is gated by one full suite run (`pr`), and a goal or release is gated by one cold, folded run (`full`), with every protection of today's gate still in force and shown to fail when it should.

## Non-goals

Those of the spec. In addition: no change to what a tier's static steps check (`fmt`, `clippy`, `crate-graph`, `cargo-deny` keep their commands); no general test-runner abstraction beyond the two runners named here; no link checker or other new documentation check.

## Context

The owner approved the plan at tier Mid, stance 0.5, and answered the five open questions; the answers are in the spec's amendments table. The owner merges every pull request. No release, tag or version bump.

Precondition, checked before planning: the previous goal is achieved with no open pull request; `main` is at caefbadaf1f00c6fc9092acd1e01fcf26f894321 and its CI run 43624044255 passed.

Findings that shape the design. Each was read in the code at the base commit unless marked otherwise.

- F1. `xtask/gate.sh` runs the suite three times. The sentinel run and the shim run discard the suite's output and status. Folding must surface those results.
- F2. Nothing counts executed tests. The script's comment on zero tests describes an incident; no check implements it.
- F3. `real-home` reports “left alone” when `~/.orchestry` existed before the run, and compares nothing.
- F4. The required CI checks are `check` and `test`, enforced by ruleset 25285267 on the default branch and on `release/**`, with no bypass actor. Classic branch protection is not the enforcement source. A workflow that is skipped as a whole leaves a required check pending; a job skipped by its own condition is reported as satisfied (GitHub's documented behaviour; to be shown on a real pull request, TV50).
- F5. Every crate depends on `orch-domain`, and `orch-cli` depends on every other crate. A change low in the graph selects most of the workspace. The five-minute target is measured on declared workloads, not promised for every change.
- F6. The timing figures in the owner's request are historical inputs. Nothing in this plan is a measurement.
- F7. An intermittent daemon-harness failure is known and undiagnosed (see the spec). Its original CI evidence is read before any diagnosis.
- F8. `.cargo/config.toml` `[env]` applies to `cargo run`. A gate started through cargo inherits `ORCHESTRY_GLOBAL_CONFIG_PATH`, which is exactly the redirect that once hid a writer from the sentinel check.
- F9. Production code falls back to the account database when `HOME` is unset. A child started with a cleared environment writes to the real home. Only `real-home` can see that.
- F10. Markdown under `crates/` is embedded in code with `include_str!`. `fixtures/` and `skills/` are read by tests of several crates. `docs/ARCHITECTURE.md` is read by `cargo xtask crate-graph` and by `xtask/tests/client_boundary.rs`. `.github/workflows/ci.yml` is read by a test in `xtask/src/linux.rs`.
- F11. `xtask` has no library target. A test in `xtask/tests/` reaches it only through the built binary. One test already builds with a nested cargo (`crates/orch-cli/tests/daemon_skew_two_binaries.rs`), so the folded run must keep cargo usable from inside a test.
- F12. The code reads `ORCH_SOCKET`, `ORCH_PROJECT_ROOT`, `ORCH_AGENT_ID` and `ORCH_TASK_ID`, which every agent session sets, and opt-in test variables such as `ORCH_BLESS_FIXTURES` and `ORCH_STRESS_SECS`. An inherited one changes what a gate run proves: a blessing run rewrites the fixtures it should compare.
- F13. `std::fs::File` locking is available on the pinned toolchain (checked with a throwaway program on macOS). Unverified: the same on Linux, expected from the standard library's documentation.
- F14. Some tests read opt-in variables from the environment. `crates/orch-cli/tests/token_eco_support/mod.rs` skips the old-reader half when `ORCH_T1_OLD_BIN` is unset and fails instead when `ORCH_T1_REQUIRE_OLD` is set; `fixtures/token-eco/matrix/run-matrix.sh` exports both. Removing every `ORCH_*` variable would remove the guard together with the opt-in (D26).
- F15. `xtask` depends on `anyhow`, `clap` and `serde_json` only: no workspace crate and no hash crate. Content identities are therefore git blob ids from `git hash-object`, which the gate needs git for anyway.
- F16. `crates/orch-infra/tests/concurrent_index.rs` starts the `orch` binary from `$CARGO_TARGET_DIR/debug/`. `orch-infra` does not depend on `orch-cli`, so the dependency graph alone does not select that test for an `orch-cli` change (D31). No other test outside `orch-cli` does this.

## Design

The lasting choices are the numbered decisions of [ADR 0013](../decisions/0013-tiered-verification-gates.md), cited here as D1–D24. This section says how they are built.

### 1. Command line

```
./xtask/gate.sh [TIER] [OPTIONS]            TIER: full (default) | pr | fast
  --plan                 print tier, steps, target and lease mode; run nothing
  --part suite|static    run one group of steps; the result is marked partial (pr and full)
  --base <rev>           fast: compare against the merge base with <rev> (default origin/main)
  --evidence <dir>       copy the redacted run record to <dir>; exit 3 if it does not qualify
  --host-label <text>    label recorded in the evidence in place of the host name
  --lease-wait <min>     how long to wait for the lease (default 30; 0 tries once)
  --no-lease <reason>    run without the lease; recorded, and the run cannot qualify
  --keep                 full: leave target/gate in place
  --warm                 full: legacy; the developer's own target, result marked "not a gate"

cargo xtask gate select --base <rev> [--format text|json|github]
                        [--changed-from <file> --metadata-from <file>]    (fixture inputs for tests)
cargo xtask gate inventory --log <file> --executables <file> [--exit <n>] [--out <file>]
cargo xtask gate evidence verify <dir> --commit <sha>
cargo xtask gate lease status
cargo xtask gate lease exec --purpose <text> [--lease-wait <min>] -- <command>...
```

Steps, in order. `full` and `pr` keep today's order and names so that results stay comparable.

| Step | Group | `full` | `pr` | `fast`, packages or all | `fast`, documentation only |
|---|---|---|---|---|---|
| `build` | suite | yes | yes | — | yes |
| `test` | suite | whole workspace | whole workspace | selected packages | — |
| `doc-test` (from PR 4, if nextest is adopted) | suite | yes | yes | selected packages | — |
| `clippy` | static | yes | yes | yes | yes |
| `fmt` | static | yes | yes | yes (first) | yes |
| `advisories` | static | required | required | — | — |
| `crate-graph` | static | yes | yes | yes | yes (the documentation check) |
| `home-hygiene` | suite | yes | yes | yes | — |
| `no-real-cli` | suite | yes | yes | yes | — |
| `real-home` | suite | last | last | last | — |

The table gains a wall-time column per step and a total. `fast` runs its cheap steps first. Until nextest is adopted, doc-tests run inside `test`, as today, and are listed separately in the inventory.

### 2. What each pull request exposes

| PR | Exposed after it merges | Not yet |
|---|---|---|
| 1 | `gate.sh` starts the Rust gate. Tiers `full` (default) and `pr`. Folded suite, judged. `--plan`, `--part`, `--evidence`, `--keep`, legacy `--warm`. `gate inventory`, `gate evidence verify`, `gate lease`. Per-step time. | `fast` answers “not available yet” with exit 2. `gate select` exists only as the stub that selects everything. CI is unchanged. |
| 2 | `fast`, `gate select`, the local documentation-only path. | CI still runs its current commands. |
| 3 | CI: selector job, `check` as anchor, `test` runs `pr --part suite`, documentation-only mode, everything on a push to `main`, and the `ambient` job on a push to `main` (D25). | The rule on `main` changes only when the owner applies it afterwards. |
| 4, only if nextest qualifies | `test` uses nextest; `doc-test` becomes its own step; serial groups. | — |
| 5 | Slow tests shortened, one slice per pull request. | — |
| 6 | AGENTS.md, README and the gate's comments describe the tiers and the rules; the fate of `--warm`. | — |

The default tier is `full` in PR 1 and stays `full` (D2). There is no later switch of the default.

When a tier becomes valid evidence:

- `full` in its folded form replaces the three-run gate only after PR 1's comparison (R4), its negative cases and its independent reviews pass. Until then the reference is the script at the base commit.
- `pr` on a developer host or on `<linux-host>` is merge evidence for `main` only after PR 3, the reviewed evidence procedure and the owner's rule change. Before that, GitHub's `check` and `test` are the merge evidence and both stay required.
- `fast` is never merge evidence.
- nextest results count only after the qualification of PR 4.

No tier certifies the pull request that introduces it.

### 3. The folded suite run (D7–D13)

Per run the gate creates one disposable root under the temporary directory, with a sentinel `home/`, an empty `cwd/` outside the workspace, `shims/` and `witness/`. It is removed at the end; on failure the listing of what appeared in `home/` and the shim log are copied to the run directory first.

Environment of the suite process: started from the gate's own environment, then `HOME` = sentinel, `PATH` = shims first, `CARGO_HOME` and `RUSTUP_HOME` explicit (default: under the real home, captured before anything moves), `RUSTUP_TOOLCHAIN` = the channel in `rust-toolchain.toml`, `CARGO_TARGET_DIR` = the tier's target, the witness variable set, and the removals of D8 as amended by D26. The names removed are printed; values are not.

Inherited variables (D26) are checked in the preflight, before the lease is taken and before any step runs. The session list is one constant in `hermetic.rs`; the `[env]` keys are read from `.cargo/config.toml`. Any other `ORCH_*` or `ORCHESTRY_*` name refuses the run and is listed. The refusal message names the way out: unset the variable, or run the opt-in test directly with cargo.

Shims: one constant list (`claude`, `codex`, `cursor`, `cursor-agent`, `agent`, `opencode`, `pi`, `grok`, `agy`). Each shim appends its name and arguments (truncated) to a log outside the sentinel and exits 127. A test ties the list to the adapters' default binary names, so an adapter cannot be added without a shim (should).

The run itself, sequential runner (PR 1). Both commands run from the outside `cwd/` and name the workspace with `--manifest-path`:

1. `cargo test --workspace --no-run --message-format=json` in that environment. This compiles and yields the map from each test executable to its package, target and kind.
2. `cargo test --workspace --no-fail-fast` in the same environment. Output and status are kept.
3. The inventory parser reads the log and the map. It produces one row per test (`kind`, `package`, `target`, `test`, `status`; an ignored test is a row with status `ignored`, so platform differences show by name) and checks D9: no failure, at least one test executed, every executable of the map present with a completed result line, counts equal to the result lines, status and inventory in agreement. Text inside a `failures:` section is not parsed as results.
4. The witness record is checked against the environment (D10).

`home-hygiene` then lists the sentinel (anything but `.cargo` and `.rustup` fails), `no-real-cli` reads the shim log, and both report “not evaluated” as a failure when the witness is absent (D11). Neither sees a child that a test starts with a cleared environment and a `PATH` or `HOME` of its own (D29).

The parser reads one log. A line that a test's child process writes into it, or one that splits a result line, makes the counts disagree; the step then fails as “inconsistent inventory”. That is a false failure to diagnose, never a pass, and it is not rerun away.

For a selected run (`fast`), the witness target is run first by itself in the same environment, because a package selection may not include `xtask`.

With nextest (PR 4) only steps 1–3 change: the expected set comes from `cargo nextest list`, the executed set from its JUnit report, they must be equal, and the gate passes `--retries 0` and `--no-tests fail` explicitly, so the design does not rest on nextest's defaults. Unverified until the pilot: that nextest reads cargo's `[env]` only from the working directory. The witness detects the opposite on every run.

`real-home` (D13, D28): the snapshot covers `~/.orchestry` to depth 4 and at most 5,000 entries. It walks level by level, each directory in name order, and records a symbolic link as an entry without following it. A complete comparison with no difference reads `unchanged (N entries)`. One cut short by a bound reads `no difference in N entries compared; partial: <bound reached>` and sets `real-home-coverage: partial` in the result. The two bounds are constants; changing them is a design change.

### 4. Targets and stale artifacts (D14–D16)

| Tier | `CARGO_TARGET_DIR` | Prepared how |
|---|---|---|
| `full` | `target/gate` | deleted, then built; deleted at exit unless `--keep`; refuses under 12 GB free |
| `pr`, `fast` | `target/gate-warm` | checked against its record; reset when untrusted |
| `full --warm` | the developer's target | untouched; result marked “not a gate” |
| building `xtask` itself | `target/gate-xtask` | by `gate.sh`: deleted when the judge identity differs from its stamp (D27) |

The warm record holds the canonical worktree root, the toolchain identity, the end time of the last build and, for every tracked and untracked source file, a content identity. `target/gate-warm` must be a real directory inside the worktree; a symbolic link is refused. The record is written around a build as D30 says: marker and identities before the first building command, record after the last one, also when compilation failed or the gate handled an interrupt; a marker found at the start resets the target.

A content identity is the git blob id of the file's content (`git hash-object`, paths passed on standard input, never through a shell). For a clean tree it equals the id in the commit's tree, which is what lets `gate evidence verify` recompute an identity from a commit.

The bootstrap in `gate.sh` (D27), in order: change to the repository root; compute the judge identity as the blob id of the sorted list of `<blob id> <path>` lines for the files of D27; if `target/gate-xtask/judge-id` is missing or differs, delete `target/gate-xtask`; remove the stamp; `cargo build -p xtask` with `CARGO_TARGET_DIR=target/gate-xtask` (an inherited value is ignored); write the stamp; start `target/gate-xtask/debug/xtask gate` with the caller's arguments and `XTASK_GATE_JUDGE_ID` set. This is all the shell does. `cargo xtask gate …` still works for the helper subcommands and for development; a tier started that way has no judge identity and cannot qualify.

### 5. Selection (D19–D20)

`gate select` reads the changes with `git diff --name-status -z -M` against the merge base, plus `git ls-files --others --exclude-standard -z`, and the graph with `cargo metadata --no-deps --format-version 1`. All subprocesses take arguments as a list. The two `--…-from` options replace both sources with files, which is how the table test runs without git or cargo.

Output (`--format json`): `schema`, `mode` (`docs-only`, `packages` or `all`), `packages` (sorted), `base`, and `reasons`, one per path: `path`, `class` (`doc`, `crate`, `global`, `coupled`, `unknown`, `error`), `selects`, `why`. `--format github` writes `mode` and `packages` as job outputs. The command exits 0 for every classification, including errors (which select everything); only a usage error exits 2.

`fast` runs one `cargo test` naming every selected package with `-p`; mode `all` runs the workspace. One invocation matters: a binary that a selected test starts by path is then built from the same tree first (D31).

The coupling table is maintained by hand and has two rows today. A document row: `docs/ARCHITECTURE.md` selects `xtask`. A package row: a change in `orch-cli` also selects `orch-infra` (F16). A package row adds the coupled package only, not that package's dependents, and adds its own reason with class `coupled`, so a path can have more than one reason. The table is the known gap of selection: a document that a test reads, or a binary that another package's test starts, that is not in the table is caught by `pr`, not by `fast`.

### 6. Lease and contention (D21–D22)

Lock file: `/tmp/r-orch-gate.lease`, created with mode 0600 and never through a symbolic link. A file owned by another account is refused with a message. A second file is the turnstile that keeps an exclusive waiter from starving. `XTASK_GATE_LEASE_DIR` moves both files; the gate's own tests always set it, so a nested gate inside the suite never touches the host lease. A run that used it cannot qualify.

`gate lease exec` holds the exclusive lease around any command, for timing series and qualification. It passes a token to its children; a gate that finds its token in the holder record proceeds and records `lease: inherited`.

Contention is sampled every 15 seconds: the one-minute load average (`/proc/loadavg` on Linux, `sysctl -n vm.loadavg` on macOS) and the count of `cargo`, `rustc`, `rustdoc`, `clippy-driver` and `cargo-nextest` processes that do not descend from the gate (`ps -axo pid=,ppid=,comm=`). The result records the maxima and `foreign-build-activity: yes|no|unavailable`. The load threshold that makes a timing sample “idle” is the performance engineer's, declared before measuring.

On interrupt the gate stops its own children, releases the lease, cleans a cold target and writes `INTERRUPTED`.

### 7. Evidence, CI and the rules change (D17, D23; R14, R15)

A qualifying `<linux-host>` run is `./xtask/gate.sh pr --evidence <dir> --host-label <label>` in a clean dedicated checkout of the exact final PR head. The evidence directory holds the result (with `judge-source-id` and `real-home-coverage`), every step log, the inventory, the lock-file digest, the tool versions, the names of the environment variables that were set or removed, and a digest manifest. The pull request links a durable copy and states the manifest's digest. Before the owner merges, a reviewer runs `gate evidence verify <dir> --commit <PR head>` in a clone that has that commit, and compares the inventory with the previous accepted one by name. The command also recomputes the judge identity from the commit and rejects evidence whose `judge-source-id` is absent or different (D27). A moved or dirty tree, a missing tool, log or inventory, an unexplained failure or a lost lease cannot qualify. Secrets are not copied.

GitHub's PR CI keeps running: tier `pr` once for code, the documentation checks only for a documentation-only change. When qualifying `<linux-host>` evidence exists the GitHub suite result is supplemental; when `<linux-host>` is offline, busy or cannot be verified, the one green GitHub run at the final head is the evidence, and nobody waits for the worker. If the Mac is unavailable, the report says “macOS not run, Mac unavailable”. A failure anywhere is kept and diagnosed; a green run on another platform does not erase it. Pushes to `main` keep the complete run. Measurements on real Linux CI remain acceptance evidence.

CI shape (PR 3): job `changes` runs `gate select --format github`; on a push to `main` the mode is forced to `all`, and the weekly schedule runs `check` alone as today. `check` has `if: always()`, fails first if `changes` did not succeed, then runs either the documentation checks or today's static commands (the pinned `cargo-deny` action stays). `test` is skipped by its job condition in documentation-only mode and otherwise runs `./xtask/gate.sh pr --part suite --no-lease "github-hosted runner"`. The checkout fetches enough history for the merge base; a missing base selects everything. Permissions stay read-only and credentials are not persisted. The test in `xtask/src/linux.rs` that mirrors `ci.yml` changes in the same pull request.

Job `ambient` (D25): runs only on a push to `main`; same setup steps as today's `test` job (toolchain, cache, tmux); then `cargo test --workspace --no-fail-fast` from the repository root, with no gate wrapper. It is never named `check` or `test` and is not added to any rule. Which directories the cache action saves once `target/gate-warm` and `target/gate-xtask` exist, and what that costs in cache size and save time, is decided and measured in PR 3.

Rules change. The ruleset is shared by `main` and `release/**`, so removing `test` from it would also drop it from `release/**`. The change is therefore a split: one rule set for `main` requiring `check`, one for `release/**` requiring `check` and `test`, everything else identical, or an equivalent reviewed configuration. The exact configuration and its rollback (the saved current ruleset) are prepared for review with PR 3. The owner applies it; the lead reads the live rules back (TV54). No agent changes a rule, merges as administrator or reports a synthetic status. Price, accepted by the owner: GitHub no longer blocks a merge to `main` on a failing suite; the owner and the reviewer check the linked evidence before every merge.

### 8. Module seams and ownership

```
xtask/gate.sh                      bootstrap only                        release engineer
xtask/src/main.rs                  registry: `Gate`, `mod gate`, `mod gate_selection`    release engineer
xtask/src/gate.rs                  arguments, dispatch, `mod` lines for gate/**          release engineer
xtask/src/gate/tier.rs             the step table (D1), parts
xtask/src/gate/steps.rs            run a step: time, log, three states, summaries
xtask/src/gate/hermetic.rs         disposable root, environment, shims, witness check
xtask/src/gate/suite.rs            the two runners (an enum, not a trait), verdict of `test`
xtask/src/gate/inventory.rs        log parser and consistency rules (pure)
xtask/src/gate/target.rs           cold and warm targets, free-space check, warm record
xtask/src/gate/real_home.rs        snapshot and comparison
xtask/src/gate/lease.rs            lock, turnstile, holder record, `lease exec`
xtask/src/gate/contention.rs       sampler
xtask/src/gate/report.rs           table, result file, run directory, evidence copy and verify
xtask/src/gate_selection.rs        the selector's whole public surface    selector builder
xtask/src/gate_selection/**        classify, graph, git                   selector builder
xtask/tests/gate_tiers.rs          TV70–TV76                              test engineer (PR 1)
xtask/tests/gate_protections.rs    TV1–TV6, TV12–TV18                     test engineer (PR 1)
xtask/tests/gate_bootstrap.rs      TV19                                   test engineer (PR 1)
xtask/tests/gate_inventory.rs      TV7–TV11                               test engineer (PR 1)
xtask/tests/gate_evidence.rs       TV40–TV47                              test engineer (PR 1)
xtask/tests/gate_lease.rs          TV60–TV68                              test engineer (PR 1)
xtask/tests/gate_witness.rs        the witness test (D10)                 test engineer (PR 1)
xtask/tests/gate_selection.rs      TV20–TV38                              second test engineer (PR 2)
```

Everything under `xtask/src/gate/**` belongs to the release engineer's tasks. Cargo discovers each file in `xtask/tests/` by itself, so no test file needs a manifest edit. `xtask/Cargo.toml`, `Cargo.toml` and `Cargo.lock` do not change in PR 1–3: the design uses only `std`, `clap`, `serde_json` and `anyhow`, which `xtask` already has.

The one coupling between the selector and the gate is this surface of `gate_selection.rs`; nothing else is used from outside:

```rust
pub struct SelectArgs { /* --base, --format, --changed-from, --metadata-from */ }
pub enum Mode { DocsOnly, Packages, All }
pub struct Reason { pub path: String, pub class: Class, pub selects: String, pub why: String }
pub struct Selection { pub mode: Mode, pub packages: Vec<String>, pub base: Option<String>, pub reasons: Vec<Reason> }
pub fn select(changes: &[Change], metadata_json: &str) -> Selection;      // pure; an error becomes Mode::All
pub fn select_from_git(repo: &Path, base: &str) -> Selection;             // git and cargo by argument list
pub fn run_cli(repo: &Path, args: SelectArgs) -> anyhow::Result<()>;      // `gate select`
```

How three builders work in disjoint paths without touching the registry:

- **Seam first (task F0, the architect's addition; needs the lead's ruling).** Before the tests and the selector start, the release engineer lands one small commit on the PR 1 branch: the `Gate` entry in `main.rs`, `gate.rs` with the argument types and every tier answering “not available yet” (exit 2), and `gate_selection.rs` as a stub with the surface above that selects everything and says so in its reason. It changes no behaviour of the existing commands. From then on the registry files belong to the release engineer alone, and `gate_selection.rs` to the selector builder alone.
- The gate tests (test engineer) and the selection tests (second test engineer) are black-box: they run the built `xtask` binary. They compile against F0 and fail until the implementation exists, which is the failing test first.
- The selector builder replaces the stub file and adds files under `gate_selection/`. Unit tests are inline. No registry edit is needed, because the stub already declared the module and the subcommand.
- **Fallback if F0 is not adopted:** the tests still compile without it (they only spawn the binary). The selector builder applies the two registry lines locally without committing them, states that in the handoff, and the release engineer adds them at integration (task I).

How the gate tests stay cheap and hermetic: each case builds a miniature workspace in a temporary directory (one crate with no dependencies, a copy of the toolchain pin, the witness test) and runs the real `xtask` binary with that directory as the working directory, `--part suite`, a temporary directory as its `HOME` (the “real home” of that run), explicit `CARGO_HOME` and `RUSTUP_HOME`, and its own `XTASK_GATE_LEASE_DIR`. Every case starts the binary with a cleared environment plus an explicit list, so nothing of an outer gate or an agent session leaks in. No case touches the developer's home, the host lease or a real agent CLI, also when the suite itself runs under an outer gate. The inventory and selection cases use files only; the evidence-verification cases use files and a temporary git repository; none of them starts cargo. The bootstrap case (TV19) copies `gate.sh` into a miniature workspace whose `xtask` package is a stand-in that prints a constant from its source and the judge identity it was given. Budget: the new test files together stay under 90 seconds on the Mac; the measured figure goes in the PR 1 report.

A complete `full` or `pr` with every step passing cannot be produced on a miniature workspace (`crate-graph` needs the real one). That wording is covered by the real runs of PR 1.

### 9. Incident ledger

Every comment in `xtask/gate.sh` that records an incident or a reason, and what happens to it. “Kept” means the behaviour is carried over unchanged.

| # | `gate.sh` lines | What the comment protects | In the new gate | Vector |
|---|---|---|---|---|
| I1 | 5–10 | A shared target across worktrees ran another branch's binaries | Kept for `full` (cold). Warm tiers: gate-owned target, inherited `CARGO_TARGET_DIR` ignored, warm record (D14, D15, D30, D3). The gate's own binary: content-checked directory (D27) | TV13–TV15, TV19 |
| I2 | 16–18 | The summary table prints whatever happens | Kept; also on `ENV-LIMITED`, refusal and interrupt | TV73, TV75 |
| I3 | 35–45 | Refuse a cold run under 12 GB free; portable `df` | Kept | TV75 |
| I4 | 47–49 | Cold target inside `target/`: ignored by git, same filesystem, no second top-level target | Kept; `gate-warm` and `gate-runs` follow the same rule | — |
| I5 | 52–61 | Cold target removed on failure and interrupt; `--keep` | Kept | TV64 |
| I6 | 69–70, 146–149 | A failing step's log was deleted; two investigations lost | Strengthened: every step's log is kept per run (D18) | TV2 |
| I7 | 74–86, 383–389 | Never run, running and finished were indistinguishable | Kept; adds `REFUSED` and `INTERRUPTED` (D18) | TV64, TV75 |
| I8 | 84–107 | Verdict without its commit; `HEAD` moved mid-run | Kept; adds dirty state at start and end, and qualification (D17) | TV43 |
| I9 | 126–130 | The table should say “1559 passed”, not “ok” | Kept; counts now come from the inventory | TV73 |
| I10 | 164–167 | cargo-deny prints four verdicts on one line | Kept | — |
| I11 | 174–176 | Name the failing tests, not only count them | Kept; names from the inventory | TV2 |
| I12 | 199–200 | `fmt --check` prints a diff; count files | Kept | — |
| I13 | 212–219 | Real `cursor` and `opencode` wrote several directories into `HOME`; anything appearing is a finding | Kept (D12); `XDG_*_HOME` removed so those writes land in the sentinel (D8) | TV3 |
| I14 | 221–223 | `.cargo` and `.rustup` are cargo's own | Kept, with its limit stated (D12) | TV1 |
| I15 | 225–228 | Run inside the workspace, `[env]` hid a writer | Kept: outside working directory. New exposure through `cargo run` closed by removal and witness (D8, D10; F8) | TV3, TV12 |
| I16 | 228–230, 241 | A moved `HOME` hid the toolchain: zero tests, reported clean | Now implemented, not only described: status judged, executed count, witness, “not evaluated” (D9–D11). `RUSTUP_TOOLCHAIN` stays explicit | TV5, TV6 |
| I17 | 260–267 | A test ran `claude --help`, `cursor-agent --list-models` and read the real `~/.codex` | Kept, with its limit stated (D29); `CODEX_HOME` removed (D8) | TV4 |
| I18 | 269–273 | A shim names the caller; an empty `PATH` cannot. Suite result ignored in that run | Shim kept. The ignored result is the fold: now judged (D7); fallout handled by R4 | TV2, TV4 |
| I19 | 275–277 | Bash dynamic scope mislabelled a table row | Gone with the port to Rust | TV73 |
| I20 | 317–339 | The real home must be as found; captured before `HOME` moves | Strengthened to a comparison that says how much it covered (D13, D28; F3, F9) | TV16, TV18 |
| I21 | 341–345 | Warn when the tree is dirty | Kept; a dirty tree cannot qualify (D17) | TV43 |
| I22 | 347–351 | Skip advisories when cargo-deny is absent, so a fresh clone can run the gate | Changed on the approved plan's word: `ENV-LIMITED`, never green. `fast` needs no extra tool (D4) | TV76 |
| I23 | 371 | `real-home` last, so it covers every step | Kept | TV16 |
| I24 | 13, 62–65 | `--warm` is fast and not a gate | Kept as legacy (D16) | TV72 |

### 10. Alternatives rejected

A1–A10 and A19–A22 are in the ADR. Design-level ones:

- A11. Include the selector's source in the test crate with `#[path]`. Changes how nested modules resolve and compiles the code twice under different lint exposure.
- A12. Give `xtask` a library target so tests can call functions. Adds a second registry file that every builder would touch.
- A13. Parse only the `test result:` lines, as today. Cannot name tests or notice a binary that never reported.
- A14. Skip a second `cargo test --no-run` pass and take executables from the `Running` lines. Two packages with the same test file name would collide.
- A15. Compare the real home by content hash. Reads files the gate has no reason to read.
- A16. Extend `real-home` to agent CLI directories. They change constantly on a host that runs agents; the step would always fail.
- A17. Use the lease for ordinary `cargo` commands through a wrapper. Not enforceable, and the owner chose measured and gating runs only.
- A18. Run static steps in parallel with the suite. Saves time and contaminates every timing.

### 11. The interface the builders share

The tests are written before the implementation and by other people, so the names below are fixed here. Anything not listed is the implementer's choice, and a test must not assert it.

Variables the gate defines:

| Variable | Set by | Meaning |
|---|---|---|
| `XTASK_GATE_WITNESS_DIR` | the gate, for the suite | the witness test writes `witness.txt` there; unset, it does nothing and passes |
| `XTASK_GATE_JUDGE_ID` | `gate.sh` | the judge identity (D27) |
| `XTASK_GATE_LEASE_DIR` | tests | directory of the lease files; a run that used it cannot qualify |
| `XTASK_GATE_LEASE_TOKEN` | `gate lease exec`, for its children | the holder's token |

`witness.txt` is lines of `key=value`: `schema=1`, `home=<HOME as the test sees it>`, `global_config_redirect=set|unset`, `path_first=<first PATH entry>`, and one `orch_var=<NAME>` per `ORCH_*` or `ORCHESTRY_*` variable present. The witness test uses `std` only, so the gate tests plant the same source file in a miniature workspace.

Lease files, in `/tmp` or in `XTASK_GATE_LEASE_DIR`: `r-orch-gate.lease` (the lock), `r-orch-gate.turnstile`, `r-orch-gate.holder` (diagnostic record).

`target/gate-result.txt` keeps its existing lines (`verdict:`, `commit:`, `tree:`, `dirty:`, `moved:`, `mode:`, `started:`, `finished:`, then the table). The value of `verdict:` starts with one of `IN PROGRESS`, `PASSED`, `FAILED`, `ENV-LIMITED`, `REFUSED`, `INTERRUPTED`; a complete tier appends ` (tier <name>)`, a partial run appends ` (part <name>)`. Added lines:

| Line | Values |
|---|---|
| `tier:` | `fast`, `pr`, `full` |
| `part:` | `suite`, `static`, or `-` for a complete tier |
| `run-dir:` | path of this run's directory under `target/gate-runs/` |
| `target:` | `cold`, `reused`, `reset: <reason>`, or `developer (not a gate)` |
| `lease:` | `exclusive`, `shared`, `inherited`, `none: <reason>`, `lost` |
| `foreign-build-activity:` | `yes`, `no`, `unavailable` |
| `real-home-coverage:` | `complete`, `partial`, or `-` when the step did not run |
| `judge-source-id:` | the identity, or `absent` |
| `qualifying:` | `yes`, or `no: <reasons>` |

Reset reasons: `no record`, `record unreadable`, `another worktree`, `toolchain changed`, `content changed without a newer modification time`, `previous build did not finish`.

Step details that tests match by substring: `no test executed`, `not evaluated`, `inconsistent inventory`, `incomplete inventory`, `witness missing`, `witness mismatch`, `not available yet`, `not a gate`, `unchanged`, `partial`, `created`. A refusal prints `REFUSED:` and its reason; for D26 the reason starts with `inherited variable` and lists the names.

Inventory file: one line per test, tab-separated `kind`, `package`, `target`, `test`, `status`, sorted bytewise. `kind` is cargo's target kind (`lib`, `bin`, `test`, `example`, `bench`) or `doc`. `status` is `ok`, `failed` or `ignored`. For `gate inventory`, `--executables` takes the JSON lines that `cargo test --no-run --message-format=json` prints; the command exits 0 when the log is accepted and 1 with the reason otherwise.

Evidence directory: `result.txt`, `steps/` (one log per step), `inventory.tsv`, `environment.txt` (names only), `tools.txt`, and `MANIFEST`, which lists `<blob id>  <path>` for every other file, sorted by path. The digest a pull request states is the blob id of `MANIFEST`. Blob ids detect a file changed after the fact; like the rest of the evidence they do not resist a producer who rewrites it (ADR, Consequences).

Selection output is the JSON of section 5 with `schema` 1.

## Test vectors

Each vector says what must happen. A negative vector must fail for the stated reason; a case that fails for another reason does not count.

Protections, on a miniature workspace (`gate_protections`):

| # | Input | Must |
|---|---|---|
| TV1 | Passing tests, nothing else | All suite steps pass; the inventory lists the fixture's tests; an empty `.cargo` in the sentinel is not a finding |
| TV2 | One failing test, clean home, no CLI | `test` FAIL naming the test; exit 1; `home-hygiene` and `no-real-cli` still evaluated and pass; the log is in the run directory |
| TV3 | A test writes `$HOME/.orchestry/global.yml` unless a redirect variable is set; the workspace's `[env]` sets it; the gate is also started with it set | `home-hygiene` FAIL listing `<HOME>/.orchestry` |
| TV4 | A test execs each shimmed name in turn | `no-real-cli` FAIL naming the CLI and its arguments, once per name |
| TV5 | A workspace with no tests | `test` FAIL “no test executed”; `home-hygiene` and `no-real-cli` FAIL “not evaluated” |
| TV6 | `RUSTUP_HOME` points at an empty directory | `test` FAIL; the other two “not evaluated”; never a clean result |
| TV12 | The witness test is absent, or reports the redirect as set | `test` FAIL naming the mismatch |
| TV13 | Control: a binary built from a passing source, the source then made failing with an older modification time, run by plain cargo on that target: passes. Then `full` on the same tree | `full` FAIL on the failing test; the planted file is gone |
| TV14 | The same plant in `target/gate-warm`, then `pr` | Result says `reset: content changed without a newer modification time`; `test` FAIL |
| TV15 | A warm target whose record names another worktree; a `target/gate-warm` that is a symbolic link; a warm target with the marker of a build that did not finish | Reset; refused; `reset: previous build did not finish` |
| TV16 | A stand-in real home: (a) `.orchestry` present and untouched; (b) a test rewrites a file in it; (c) absent, a test creates it | (a) pass “unchanged”; (b) FAIL listing the path; (c) FAIL “created” |
| TV17 | The gate is started with, in turn: `ORCH_T1_REQUIRE_OLD`, `ORCH_T1_OLD_BIN`, `ORCH_BLESS_FIXTURES`, `ORCH_STRESS_SECS`, a name the code does not know (`ORCH_SOME_NEW_KNOB`). Then with every session variable of D26 set at once | Each of the first: exit 1, `REFUSED`, the name listed and its value absent from all output, no suite process started. The last: the run proceeds, the names are printed as removed, and the witness lists no `orch_var` |
| TV18 | A stand-in real home with more entries than the count bound: (a) nothing changes; (b) a test rewrites a top-level file; (c) in a home that exceeds only the depth bound, a test creates a file below the bound, in a directory at the bound | (a) pass, detail says `partial` and the number compared, never “unchanged”, result has `real-home-coverage: partial`; (b) FAIL listing the file; (c) FAIL listing the directory |

Bootstrap, on a miniature workspace with a stand-in `xtask` (`gate_bootstrap`):

| # | Input | Must |
|---|---|---|
| TV19 | (a) First run of the copied `gate.sh`. (b) Control: the stand-in's source is changed and given an older modification time; plain `cargo build` in `target/gate-xtask` does not rebuild. Then `gate.sh` again. (c) The stamp is deleted, then `gate.sh`. (d) An inherited `CARGO_TARGET_DIR` pointing elsewhere | (a) builds in `target/gate-xtask`, writes the stamp, the binary receives the identity; (b) the directory is rebuilt from nothing and the binary shows the new source; (c) rebuilt from nothing; (d) ignored, nothing is written there |

Inventory, on prepared logs (`gate_inventory`):

| # | Input | Must |
|---|---|---|
| TV7 | A consistent passing log and its executables map | Accepted; rows equal the tests |
| TV8 | A `FAILED` test line with exit 0 | Rejected, “inconsistent” |
| TV9 | Exit 101 and no failed test in the log | Rejected, “inconsistent” |
| TV10 | An executable in the map with no result line | Rejected, “incomplete” |
| TV11 | Two packages with the same test file name; result-like lines inside a `failures:` section; doc-tests | Distinct identities; the section is ignored; doc-tests have kind `doc` |

Selection (`gate_selection`), on a metadata fixture unless marked git:

| # | Changed paths | Must select |
|---|---|---|
| TV20 | source of a crate nothing depends on | that package |
| TV21 | `docs/RUNBOOK.md`, `README.md` | no tests, mode `docs-only` |
| TV22 | source of `orch-domain` | it and every member that depends on it |
| TV23 | source of a mid-graph crate | it and its transitive dependents |
| TV24 | a crate that another uses only as a development dependency | both |
| TV25 | a document and a crate source | the crate's packages; the document does not narrow it |
| TV26 | `Cargo.lock` | everything |
| TV27 | the root `Cargo.toml`; a member's `Cargo.toml` | everything |
| TV28 | `rust-toolchain.toml`; `.cargo/config.toml`; `clippy.toml`; `deny.toml` | everything |
| TV29 | a file under `xtask/`; under `.github/`; under `.config/` | everything |
| TV30 | a file under `fixtures/`; `skills/`; `examples/` | everything |
| TV31 | `Makefile`; `newdir/file` | everything, class `unknown` |
| TV32 | a Markdown file under `crates/<member>/` | that package and its dependents, not `docs-only` |
| TV33 | a rename from one crate to another; a rename from `docs/` into a crate; a deleted crate file; a path under `crates/` that no member owns | both packages; the package; the package; everything |
| TV34 | `docs/ARCHITECTURE.md` | package `xtask` |
| TV35 | malformed metadata; metadata without members; a malformed change record; a path that is not UTF-8 | everything, class `error` |
| TV36 | no changed path | everything |
| TV37 (git) | an unknown base; an uncommitted edit; an untracked file; a symbolic link or submodule change; a path with a space, a newline and a leading dash | everything; included; included; everything; handled as data |
| TV38 | source of `orch-cli`; source of `orch-infra` | `orch-cli` and `orch-infra`, the second with a reason of class `coupled`; `orch-infra` and its dependents (which include `orch-cli`), with no `coupled` reason |

Every row also asserts that the reasons name each path and its class.

Evidence (`gate_evidence`):

| # | Input | Must |
|---|---|---|
| TV40 | A well-formed evidence directory and its commit | Verified |
| TV41 | One log edited after the manifest was written | Rejected: digest |
| TV42 | A different expected commit | Rejected: commit |
| TV43 | Dirty at the end; `HEAD` moved | Rejected |
| TV44 | A partial run; an `ENV-LIMITED` step; a failed step | Rejected |
| TV45 | No lease; a shared lease; a lease directory override; a lost lease | Rejected |
| TV46 | An evidence copy from a miniature run | Contains neither the repository root, nor the home, nor the temporary directory path, nor a host name; exit 3 because the run is partial |
| TV47 | Evidence whose `judge-source-id` is that of another commit's `xtask/`; evidence with `judge-source-id: absent`; evidence whose id matches the expected commit | Rejected: judge; rejected: judge; accepted on this point |

CI and rules, on real executions (PR 3 and after):

| # | Case | Must |
|---|---|---|
| TV50 | A documentation-only pull request | `check` green in documentation mode; `test` skipped; both required checks satisfied |
| TV51 | A code pull request | `check` and `test` each run once; `test` runs the `pr` suite part |
| TV52 | The `changes` job does not succeed (asserted on the workflow file by a test, and shown once on a throwaway branch) | `check` fails; the pull request cannot merge |
| TV53 | A push to `main` | Everything runs; no selection |
| TV54 | After the owner's rule change | Read back: `main` requires `check`; `release/**` requires `check` and `test`; all other rules unchanged; the saved rollback restores the old state |
| TV55 | The `ambient` job (asserted on the workflow file by a test, and shown on the first push to `main` after PR 3) | It runs on a push to `main` and on no pull request; its command is `cargo test --workspace --no-fail-fast` with no gate wrapper; it is not named `check` or `test`; the push shows it ran beside `test` |

Lease and contention (`gate_lease`), with an isolated lease directory:

| # | Case | Must |
|---|---|---|
| TV60 | Exclusive while exclusive is held, wait 0 | Exit 75 naming the holder; nothing ran |
| TV61 | Shared while shared is held | Both proceed |
| TV62 | Exclusive waiting on a shared holder; a new shared request arrives | The new request queues behind the exclusive waiter |
| TV63 | The holder is killed with SIGKILL | The lease is free at once; the stale record is replaced; no signal is sent |
| TV64 | The holder gets SIGINT or SIGTERM | Lease released; cold target removed; result `INTERRUPTED` |
| TV65 | The lock file is replaced while held | The holder records the lease as lost; the run does not qualify |
| TV66 | The lock path is a symbolic link; the file has another owner | Refused with the reason; not followed |
| TV67 | A gate under `lease exec`; the same without the token | `lease: inherited`; waits |
| TV68 | A foreign process named `cargo` during the run; load data unavailable | `foreign-build-activity: yes`; `unavailable`, and the sample is not a valid measurement |

Tiers (`gate_tiers`):

| # | Case | Must |
|---|---|---|
| TV70 | `--help` | Names the three tiers, `pr` as the merge condition, `full` as the goal and release gate and as the default |
| TV71 | `--plan` with no tier; with each tier | `full`; each tier's steps, target and lease mode as in section 1 |
| TV72 | An unknown tier or option; `fast` before PR 2; `--warm` with `pr` | Exit 2 with the reason |
| TV73 | A miniature run | Each row has verdict, name, wall time and detail; a total; the result file has `tier`, `part`, `run-dir` and the times |
| TV74 | `--part suite`; `full --warm` | The result says `part: suite` and never `PASSED (tier`; the legacy run says “not a gate” and cannot qualify |
| TV75 | A preflight refusal | Exit 1; the result file says `REFUSED`; the table is printed |
| TV76 | `cargo-deny` not on `PATH`, `--part static` | The `advisories` row is `ENV-LIMITED` with the install command; the run is not green |

## Requirements map

| Req | Decisions | Vectors | Tasks | PR |
|---|---|---|---|---|
| R1 | D1, D2, D4–D6, D18 | TV70–TV76 | F0, C, F | 1 |
| R2 | D7–D12, D14, D26 | TV1–TV12, TV17 | C, F | 1 |
| R3 | D8, D10–D15, D26–D30 | TV3, TV4, TV13–TV19 | C, F | 1 |
| R4 | D7, D9 | the comparison of the two inventories | F | 1 |
| R5 | D19, D20, D31 | TV20–TV38 | G, H, I | 2 |
| R6 | D19, D23 | TV21, TV25, TV32, TV34, TV50 | G, H, I, J | 2, 3 |
| R7 | D1, D6, D23, D25 | TV51–TV53, TV55 | F, J | 1, 3 |
| R8 | D24 | measured records | K, L | 4 |
| R9 | — | per-binary measurements and the changed-test ledger | D, M | 5 |
| R10 | D2, D16 | fact check of the final documents | N | 6 |
| R11 | D9, D18 | inventories and timing records | D, K, O | report |
| R12 | D1, D15 | measured wall times | D, O | report |
| R13 | — | each PR's diff and status | all | all |
| R14 | D17, D27 | TV40–TV47, one real run | C, F, J | 1, 3 |
| R15 | D23 | TV54 | J, owner, lead | after 3 |
| R16 | D21, D22 | TV60–TV68 | C, F | 1 |

## Tasks

Owners are roles. Each task runs in its own worktree and branch, never shares a `CARGO_TARGET_DIR`, and commits only the paths in its scope. Readers review a frozen commit and write only their own report.

| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| A | Design: spec, plan, ADR (architect) | R1–R16 | `docs/specs/2026-11-14-tiered-checks.md`, `docs/plans/2026-11-14-tiered-checks.md`, `docs/decisions/0013-tiered-verification-gates.md` | approval | links resolve; `git diff --check` | 1 |
| B | Recover the original CI failure of the daemon-harness test; hypotheses; bounded reproduction (triage) | R7, R9 | own report | approval | the original log is cited | evidence |
| D | Baseline measurements and attribution of every binary above 30 s (performance engineer) | R9, R11, R12 | own report and timing records | approval; exclusive lease while measuring | records regenerate from the logs | evidence |
| E | Challenge: can one folded run replace three without a false green (red team) | R2, R3 | own report | A | verdict on the frozen design | evidence |
| F0 | Registry seam: `Gate` entry, argument types, stubs (release engineer) | R1 | `xtask/src/main.rs`, `xtask/src/gate.rs`, stub `xtask/src/gate_selection.rs` | A, E disposition | `cargo test -p xtask`; `cargo xtask crate-graph` unchanged | 1 |
| C | Gate tests, written to fail first (test engineer) | R1–R3, R14, R16 | `xtask/tests/gate_tiers.rs`, `gate_protections.rs`, `gate_bootstrap.rs`, `gate_inventory.rs`, `gate_evidence.rs`, `gate_lease.rs`, `gate_witness.rs` | F0 | each negative case fails for its stated reason against F0 | 1 |
| F | Folded gate, timings, lease, evidence (release engineer) | R1–R4, R7, R14, R16 | `xtask/gate.sh`, `xtask/src/gate.rs`, `xtask/src/gate/**` | C, D baseline | C's tests pass; the old and the folded run compared at one commit (R4); `./xtask/gate.sh full` on Mac and native Linux | 1 |
| G | Selector (senior engineer) | R5, R6 | `xtask/src/gate_selection.rs`, `xtask/src/gate_selection/**` | F0 | inline unit tests; H's table | 2 |
| H | Selection table tests, from the vectors and independent of G (second test engineer) | R5, R6 | `xtask/tests/gate_selection.rs` | F0 | fails against the stub except the select-everything rows | 2 |
| I | Integrate the selector with `fast` (release engineer) | R5, R6, R12 | `xtask/src/gate.rs`, `xtask/src/gate/**` | F, G, H | `cargo test -p xtask --test gate_selection`; `./xtask/gate.sh fast --base <base>` on the declared workloads | 2 |
| J | CI workflow, with the `ambient` job; proposed rule configuration and rollback, for review (release engineer) | R6, R7, R14, R15 | `.github/workflows/ci.yml`, `xtask/src/linux.rs` (the test that mirrors it) | I | TV50–TV53 on real runs; TV55 | 3 |
| K | nextest pilot against the sequential baseline; 20-run qualification per platform; a series stops at its first failure, which is kept and diagnosed, and a fixed candidate starts a new series (performance engineer) | R8 | own report and records | F; native Linux; exclusive lease | records per platform | evidence |
| L | Adopt nextest (release engineer), only if K qualifies | R8 | `.config/nextest.toml`, `xtask/src/gate/suite.rs`, `xtask/src/gate/tier.rs`, `.github/workflows/ci.yml` | K, J | same inventory by name; the 20-run logs; dependency review and pinned version | 4 |
| M | Slow tests, one slice per file group (senior engineers, separate tasks) | R9 | the exact test files and trivial clock seams named after D and B | D, B | a failing regression first; assertions and timeouts unchanged; independent check by a test engineer | 5 |
| N | Rules and documentation (technical writer) | R10 | `README.md`, `AGENTS.md` (after the owner's word), the spec and plan status, the comments in `xtask/gate.sh` | implemented interfaces; all decisions | every command runs as written; fact check | 6 |
| O | Final validation, no source edits (release engineer) | R11, R12 | — | all accepted slices | `./xtask/gate.sh full` on Mac; native Linux CI; timings and inventories checked against the logs | final |

Order: A → E → F0 → C, G, H in parallel → F → I → J → (L) → N → O. B and D are independent inputs; M follows their findings. F0 is the one change to the approved order; without it the fallback of section 8 applies.

One heavy build or measurement per host at a time. Measurement windows hold the exclusive lease; documentation, review and reading continue.

## Status

| Item | State |
|---|---|
| A | design v2 committed: challenged once, reconciled; awaiting the lead's rulings on the open items |
| all others | pending |

## Test plan

The spec's acceptance checks; the vectors above; the CI set on every pull request. `./xtask/gate.sh full` on macOS and on native Linux for PR 1 and at the end, because the work changes process handling and the gate itself. PR 1 also runs the gate of the base commit once at the same tree, for the comparison of R4. Both runs of that comparison hold the exclusive lease on an otherwise idle host. One run on each side cannot tell a failure caused by the fold from an intermittent one, so every test whose result differs is then run by itself in both environments. The report lists every difference with what those runs showed, including the ones that did not repeat; an intermittent one is triaged under R7. None of this is a rerun to obtain a pass. Timing and qualification runs hold the exclusive lease on an otherwise idle host. Reviewers use bounded disposable probes; no probe uses a real home or a real agent CLI. No user interface changes, so no tmux end-to-end test is added.

Reviews: each pull request gets independent code, security and resilience reviews of what it touches, at a frozen commit. Security covers subprocesses, paths, the untrusted diff and CI; resilience covers failure, interruption, cleanup, the lease, kept diagnostics and rollback. A fact check covers timings, inventories and public claims. One red-team pass challenges the fold (task E). A blocking finding is fixed and reviewed again or goes to the owner; it is not overruled.

Price, from the approved plan: at least 40 nextest qualification runs (20 per platform), plus baselines, pilots, the fold comparison, the negative cases, CI validation and the final gates. At an illustrative 10 minutes each, qualification alone is 400 machine-minutes; that is arithmetic, not a measurement.

## Rollback / compatibility

No product data, on-disk format or protocol changes. Each pull request is reverted by itself, in reverse order of dependency; reverting PR 1 restores the three-run script. The rules change has its own rollback: the saved ruleset. `target/gate-result.txt` keeps its place and its three states and gains lines; a reader that looks for `verdict:` still works. `--warm` and `--keep` keep working. The new directories (`target/gate-warm`, `target/gate-runs`) are inside `target/` and ignored by git. No test is deleted and no failure is hidden to meet a target.

## Docs to update

- PR 1: the header comment of `xtask/gate.sh`; `docs/decisions/README.md` (index row for 0013). One sentence of AGENTS.md becomes untrue in PR 1 (“its `advisories` step silently passes when cargo-deny is missing”); see the open items.
- PR 3: the comment at the top of `.github/workflows/ci.yml`.
- PR 6: AGENTS.md (pre-PR check, workflow, release step, roles), README “Development”, this plan and the spec set to `done`. The text on what each tier proves must carry the stated limits: no tier runs the ambient configuration and the `ambient` job reports after the merge (D25); a gate refuses opt-in test variables (D26); `real-home` may be `partial` (D28); `no-real-cli` and `home-hygiene` do not see a child with its own environment (D12, D29). `docs/RUNBOOK.md` has no developer check section today and needs none.

## Open items

For the lead; none blocks the design challenge.

1. F0 (section 8): adopt the seam-first commit, or keep the approved order with the fallback.
2. The test engineer's scope grows from two files to seven (section 8). They are disjoint from every other task.
3. AGENTS.md in PR 1: correct the one sentence about `advisories` there (a factual fix in an owner-merged file), or carry the mismatch until PR 6.
4. D13 can fail because another `orch` process changed the real global directory during a gate. If that proves frequent, it needs the owner: accept manual attribution, or run gates with `orch` idle.
5. D8 and D26 keep `ORCH_*` out of the suite's environment. A test that relied on an inherited variable will show up in the R4 comparison. The session list of D26 is a constant; when `orch` starts exporting a new variable to agent sessions, a gate started from such a session is refused and names it, and the list gains a line.
6. Unverified and settled only by running: GitHub's treatment of a job skipped by condition under this ruleset (TV50); nextest's reading of `[env]` (section 3); file locking and `ps` on Linux (F13, section 6); the cost of a from-nothing build of `xtask` (D27); that creating a file changes its parent directory's modification time on both platforms (TV18 c).
7. D25: the `ambient` job runs on every push to `main`. Alternative A19 runs it weekly instead. Either answers the challenge; the lead picks, and the job is new work in task J.
8. A suite that hangs holds the exclusive lease, and the design has no suite deadline. A waiter exits 75 after its wait and names the holder. Whether the gate needs its own deadline is for the resilience review of PR 1.
9. The inventory parser reads one merged log (section 3). It fails closed. Whether to parse two streams is for the code review of PR 1.
10. PR 3 decides and measures what the CI cache saves (section 7).
11. Possible follow-up, not in this plan: a source check that flags a `PATH` with host install directories in test code (D29).
