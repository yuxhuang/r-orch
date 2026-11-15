# Tiered verification checks — plan

status: active
owner: orchestrator
date: 2026-11-14
spec: [../specs/2026-11-14-tiered-checks.md](../specs/2026-11-14-tiered-checks.md)
decision record: [ADR 0013](../decisions/0013-tiered-verification-gates.md) (accepted at revision 2); [ADR 0014](../decisions/0014-gate-target-recovery-and-selection-coupling.md) is its proposed partial successor and is not yet accepted
design revision: v2 (reconciled with the design challenge), plus the staging amendments of section 12, the decision reconciliation of ADR 0014 and its correction (sixth amendment)

Changes from v1: ADR decisions D25–D31 and alternatives A19–A22; findings F14–F16; sections 3, 4, 5, 7 and 8 follow those decisions; new section 11 (the interface the builders share); new vectors TV17–TV19, TV38, TV47 and TV55, and a third case in TV15; the comparison procedure for R4 in the test plan; open items 2 and 5 updated, 7–11 added. No requirement, task id or existing vector number changed.

Staging amendments (after the seam commit F0): new section 12 records which obligations are required in PR 1 and which in PR 2, the order in which the host runs the work, and what is still held. Sections 2, 6, 8 and the task and status tables carry short pointers to it; the notes of TV61–TV64 and TV72 and the "Verify" wording of tasks C and H are corrected. Requirement, vector and task ids are unchanged.

Second staging amendment (after the C repair and the G and H validation): section 11 gains the provisioning and verifier-exit clarification for task C; section 12 gains the verification-state ledger, the refreshed status of C, G and H, and the obligations that stay open (target-path comparison, the sampler seam, the signal and lifecycle hold, and the broader D31 coupling). The status and task tables and the TV38 and TV68 notes are corrected. Requirement, vector and task ids are unchanged, and no requirement is relaxed.

Third staging amendment (after the security and resilience delta reviews of the repaired C): section 12 records the review dispositions, the safety holds K-1 to K-4 on recovery, the holder identity K-5, the unchosen numeric grace of RS5, the limit of scratch retention, and the future owners of the decision reconciliation and of the runtime. These are holds and obligations, not an implemented recovery and not a change to an accepted decision. Short pointers are added at the warm-record text of section 4, the TV15, TV63 and TV64 rows, the status table and the open items. Requirement, vector and task ids are unchanged, and no requirement is relaxed.

Fourth staging amendment (after the source corrections of C at `373ea896c3d6149fa5c9c43fc12f22228d57033c`): the stale wording that called N1, SR-10 and G-1 pending is replaced by their status, accepted by a static reading only, with all eleven new controls and every mutation unrun and not compiled as of this amendment. Section 12 states that cold-target removal after a handled interrupt (TV64) also needs independently established cessation of the target's writers, as warm reset, reuse and publication already do. The status table, the TV64 row and open items 13 and 14 carry the pointer. Requirement, vector and task ids are unchanged, and no requirement is relaxed.

Fifth amendment (decision reconciliation, by the architect; changes from the fourth): [ADR 0014](../decisions/0014-gate-target-recovery-and-selection-coupling.md), proposed, supersedes named clauses of ADR 0013 decisions 30 and 31 and qualifies decisions 14, 15 and 21. Its decisions are cited here as D32–D40. The operative text that still prescribed the superseded behaviour is corrected in place: section 4 (the warm record and the target table), section 5 (the coupling trigger and the build order), section 6 (the holder and the cold target on interrupt), rows I1 and I5 of section 9, the reset reasons of section 11, rows RS2 and RS5 and the ordering paragraph of section 12, and the vectors TV15, TV38, TV63 and TV64. Section 12 gains the fifth-amendment record: the recovery cases RC1–RC10, the edits that the frozen test and selector sources will need (listed, not made and not authorized), the runtime prerequisites of task F, the current evidence for the eleven filesystem controls of C, and the limit of what this reconciliation achieves. The records of the first four amendments are kept as written; where one of their sentences is no longer current, a pointer says so. The requirements map, the task and status tables, "Docs to update" and open items 12–18 follow. Requirement, vector and task ids are unchanged; no requirement or lifecycle case is relaxed; no numeric grace is chosen. This amendment does not make the gate workable on a macOS host where all agents share one account (section 12).

Sixth amendment (correction of the fifth, by the architect; changes from the fifth): the security and resilience reviewers each passed the fifth amendment with conditions, and the lead ruled on every point. This amendment makes those corrections and nothing else. ADR 0014 is corrected to its revision 2 and stays proposed. ADR 0013's status line now says `accepted` at revision 2, which is what the lead's records already held; its body is unchanged. Corrected in place: the target table and the recovery paragraph of section 4; the interrupt paragraph of section 6; the qualification sentence of section 7; the `target:` values, the reset reasons and a new kept-target paragraph in section 11; rows RS2, RS3 and RS5, the build-order case and the fifth-amendment record of section 12 (RC2, RC6, RC7 and RC8 changed, RC11–RC13 added; C6 changed, C8 and C9 added; P4, P5 and P7 rewritten); the notes of TV13 and TV64; the requirements map; status rows A and C; open items 15–17. Section 12 gains the sixth-amendment record, which also gives the reason for this further review round. The five earlier header paragraphs above are kept as written. Where the fifth says that ADR 0014 "supersedes" or "qualifies" a decision, read "is proposed to": nothing in ADR 0013 is superseded until ADR 0014 is accepted, and ADR 0014 would also qualify decision 17. Requirement, vector and task ids are unchanged; no requirement, vector or lifecycle case is relaxed; no number is chosen; no runtime hold is lifted.

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

The lasting choices are the numbered decisions of [ADR 0013](../decisions/0013-tiered-verification-gates.md), cited here as D1–D31, and of [ADR 0014](../decisions/0014-gate-target-recovery-and-selection-coupling.md) (proposed), cited as D32–D40. Where ADR 0014 would supersede a clause of D30 or D31, this plan follows the lead's rulings that ADR 0014 carries (section 12); ADR 0014 itself is proposed and not yet accepted. This section says how they are built.

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
| 1 | `gate.sh` starts the Rust gate. Tiers `full` (default) and `pr`. Folded suite, judged. `--plan`, `--part`, `--evidence`, `--keep`, legacy `--warm`. `gate inventory`, `gate evidence verify`, `gate lease` (exclusive and inherited holders). Per-step time. A single writer per warm target, tested through the entries PR 1 has (section 12). | `fast` answers “not available yet” with exit 2, so there is no shared-lease requester and TV61, TV62 and the shared-fast case of RS1 are not executable (section 12). `gate select` exists only as the stub that selects everything. CI is unchanged. |
| 2 | `fast`, `gate select`, the local documentation-only path, and the D31 coupling correction (section 12). `fast` is the first real shared-lease requester, so TV61, TV62 and the shared-fast case of RS1 are required and executed here (section 12). | CI still runs its current commands. |
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
| `full` | `target/gate` | deleted, then built; deleted at exit unless `--keep`; refuses under 12 GB free. Each deletion needs established cessation of the writers of an earlier gate use (D32, D36); without it the target is left and the run says so. A `target/gate` that exists at the start is deleted only on the affirmative provenance of D33; without it the run is refused, which is every such start until P5 exists (sixth amendment) |
| `pr`, `fast` | `target/gate-warm` | checked against its record; reset when untrusted. A reset or a reuse needs established cessation of the previous use's writers (D32); without it the run refuses. A target whose marker or record is missing, unreadable, corrupt or incomplete has unknown history and is refused as well (D33, sixth amendment) |
| `full --warm` | the developer's target | untouched; result marked “not a gate” |
| building `xtask` itself | `target/gate-xtask` | by `gate.sh`: deleted when the judge identity differs from its stamp (D27) |

The warm record holds the canonical worktree root, the toolchain identity, the end time of the last build and, for every tracked and untracked source file, a content identity. `target/gate-warm` must be a real directory inside the worktree; a symbolic link is refused. The record is written around a build (D30 as amended by D35). The marker and the identities are written before the first building command. The record is written, and the marker removed, only when the cessation of every writer of this use has been independently established (D32, D33). That is the condition after a successful build, after a failed compilation and after an interrupt the gate handled alike; the return of the last building command, the exit of the gate's direct children, end of file on a channel or the end of a bounded wait is not that condition. The end time in the record is taken when cessation is established. Where it is not established, no record is written and the marker stays (D34).

A run that finds a marker at its start does not reset the target by itself. It refuses, changes nothing and says why, unless the cessation of the earlier use's writers is independently established; only then does it reset, with `reset: previous build did not finish`. The same precondition applies to every other reset and to reuse. A missing recorded process id, the marker's state, an expired wait or an operator's acknowledgement does not meet it (D33, D34). There is no automatic recovery after a hard kill and none after a normal finish that left its writers unresolved; recovery stays refused until cessation is established. Sixth amendment (D35): this is the rule for every use that ended without established cessation, whether by a normal finish, a failed compilation, a handled interrupt or the death of the holder. No record was completed for it, so once its cessation is established the next run resets, and it never reports `reused`. A warm target that exists with neither marker nor record, or with either of them unreadable, corrupt or incomplete, has unknown history: that is not a reset with `no record` or `record unreadable` but a refusal, unless the provenance of D33 is affirmatively established (section 11). The earlier wording of this paragraph (a marker "resets the target"; the record written "after the last" command) is superseded with the clauses of D30 it restated.

A content identity is the git blob id of the file's content (`git hash-object`, paths passed on standard input, never through a shell). For a clean tree it equals the id in the commit's tree, which is what lets `gate evidence verify` recompute an identity from a commit.

The bootstrap in `gate.sh` (D27), in order: change to the repository root; compute the judge identity as the blob id of the sorted list of `<blob id> <path>` lines for the files of D27; if `target/gate-xtask/judge-id` is missing or differs, delete `target/gate-xtask`; remove the stamp; `cargo build -p xtask` with `CARGO_TARGET_DIR=target/gate-xtask` (an inherited value is ignored); write the stamp; start `target/gate-xtask/debug/xtask gate` with the caller's arguments and `XTASK_GATE_JUDGE_ID` set. This is all the shell does. `cargo xtask gate …` still works for the helper subcommands and for development; a tier started that way has no judge identity and cannot qualify.

### 5. Selection (D19–D20)

`gate select` reads the changes with `git diff --name-status -z -M` against the merge base, plus `git ls-files --others --exclude-standard -z`, and the graph with `cargo metadata --no-deps --format-version 1`. All subprocesses take arguments as a list. The two `--…-from` options replace both sources with files, which is how the table test runs without git or cargo.

Output (`--format json`): `schema`, `mode` (`docs-only`, `packages` or `all`), `packages` (sorted), `base`, and `reasons`, one per path: `path`, `class` (`doc`, `crate`, `global`, `coupled`, `unknown`, `error`), `selects`, `why`. `--format github` writes `mode` and `packages` as job outputs. The command exits 0 for every classification, including errors (which select everything); only a usage error exits 2.

`fast` names every selected package with `-p`; mode `all` runs the workspace. A binary that a selected test starts by path must be built from the same tree before that test starts, and the gate sets that order itself (D40): it first completes a build that brings the binary up to date in the tier's target, and starts a test process only after that build succeeded. The order is not inferred from how cargo schedules one invocation, and a binary already present in a warm target is not trusted. This holds for every tier that runs such a test, the workspace runs of `pr` and `full` included. Section 3's two commands (`--no-run`, then the run) are an order the gate sets; that the first of them builds the `orch` binary for a `-p` selection is an assumption until the two-marker probe of section 12 shows it. If it does not, task I adds an explicit build of the binary before the tests. The exact commands are the implementation's.

The coupling table is maintained by hand and has two rows today. A document row: `docs/ARCHITECTURE.md` selects `xtask`. A package row (D39, F16): after the normal reverse-dependency closure, a selection that contains `orch-cli` and not `orch-infra` also selects `orch-infra`. The row therefore fires for a change in `orch-cli` and for a change in any crate that `orch-cli` depends on and `orch-infra` does not. A package row adds the coupled package only, not that package's dependents, and adds its own reason with class `coupled`, so a path can have more than one reason. A change in `orch-infra` itself selects it and its dependents through the normal closure, with no `coupled` reason. The table is the known gap of selection: a document that a test reads, or a binary that another package's test starts, that is not in the table is caught by `pr`, not by `fast`.

The selector frozen at G fires the package row only for a change in `orch-cli` itself, which is the superseded trigger of D31. Changing it to the trigger above, the added TV38 case and the build-order evidence are PR 2 work (section 12); the frozen G and H are not edited by this plan.

### 6. Lease and contention (D21–D22)

Lock file: `/tmp/r-orch-gate.lease`, created with mode 0600 and never through a symbolic link. A file owned by another account is refused with a message. A second file is the turnstile that keeps an exclusive waiter from starving. `XTASK_GATE_LEASE_DIR` moves both files; the gate's own tests always set it, so a nested gate inside the suite never touches the host lease. A run that used it cannot qualify.

`gate lease exec` holds the exclusive lease around any command, for timing series and qualification. It passes a token to its children; a gate that finds its token in the holder record proceeds and records `lease: inherited`. Obligation RS7 (section 12) limits this: the holder record is a diagnostic, so the token counts only while the exclusive lock is still held by a live holder; if a non-blocking lock attempt succeeds, the holder is gone and the gate must take the lease normally.

Contention is sampled every 15 seconds: the one-minute load average (`/proc/loadavg` on Linux, `sysctl -n vm.loadavg` on macOS) and the count of `cargo`, `rustc`, `rustdoc`, `clippy-driver` and `cargo-nextest` processes that do not descend from the gate (`ps -axo pid=,ppid=,comm=`). The result records the maxima and `foreign-build-activity: yes|no|unavailable`. The load threshold that makes a timing sample “idle” is the performance engineer's, declared before measuring.

The holder of the lease is the process the caller started, which may have replaced itself by `exec`; it alone owns the lock's file descriptor, no descendant inherits it and no helper keeps it after the holder is killed (D37; section 12, K-5).

On interrupt the gate stops its own children, releases the lease and writes `INTERRUPTED`. Obligation INV-T (section 12) bounds every wait the gate performs, including this one; the bound has no number yet (D38). It removes the cold target only when the cessation of the target's writers has been independently established (D32, D36). Otherwise it leaves `target/gate` in place, writes no warm record, lists what survived or could not be attributed, and says that the target was kept and why: the result carries `target: kept (<reason>)` and `qualifying: no` with that reason (D34, section 11). A handled signal, the exit of the direct children and the end of the bounded wait do not authorize the removal.

### 7. Evidence, CI and the rules change (D17, D23; R14, R15)

A qualifying `<linux-host>` run is `./xtask/gate.sh pr --evidence <dir> --host-label <label>` in a clean dedicated checkout of the exact final PR head. The evidence directory holds the result (with `judge-source-id` and `real-home-coverage`), every step log, the inventory, the lock-file digest, the tool versions, the names of the environment variables that were set or removed, and a digest manifest. The pull request links a durable copy and states the manifest's digest. Before the owner merges, a reviewer runs `gate evidence verify <dir> --commit <PR head>` in a clone that has that commit, and compares the inventory with the previous accepted one by name. The command also recomputes the judge identity from the commit and rejects evidence whose `judge-source-id` is absent or different (D27). A moved or dirty tree, a missing tool, log or inventory, an unexplained failure or a lost lease cannot qualify. A run that kept its target because the cessation of its writers was unresolved cannot qualify either, in any tier (D34). Secrets are not copied.

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

How three builders work in disjoint paths without touching the registry (the order the host runs them in is section 12):

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
| I1 | 5–10 | A shared target across worktrees ran another branch's binaries | Kept for `full` (cold). Warm tiers: gate-owned target, inherited `CARGO_TARGET_DIR` ignored, warm record published only at established cessation (D14, D15, D30 as amended by D32–D36, D3). The gate's own binary: content-checked directory (D27) | TV13–TV15, TV19 |
| I2 | 16–18 | The summary table prints whatever happens | Kept; also on `ENV-LIMITED`, refusal and interrupt | TV73, TV75 |
| I3 | 35–45 | Refuse a cold run under 12 GB free; portable `df` | Kept | TV75 |
| I4 | 47–49 | Cold target inside `target/`: ignored by git, same filesystem, no second top-level target | Kept; `gate-warm` and `gate-runs` follow the same rule | — |
| I5 | 52–61 | Cold target removed on failure and interrupt; `--keep` | Kept, with a precondition the script never had: the removal needs established cessation of the target's writers (D32, D36). Without it the target is left and the result says so; a target removed under a live writer could be recreated and read as fresh | TV64 |
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
| `target:` | `cold`, `reused`, `reset: <reason>`, `kept (<reason>)`, or `developer (not a gate)` |
| `lease:` | `exclusive`, `shared`, `inherited`, `none: <reason>`, `lost` |
| `foreign-build-activity:` | `yes`, `no`, `unavailable` |
| `real-home-coverage:` | `complete`, `partial`, or `-` when the step did not run |
| `judge-source-id:` | the identity, or `absent` |
| `qualifying:` | `yes`, or `no: <reasons>` |

Reset reasons: `no record`, `record unreadable`, `another worktree`, `toolchain changed`, `content changed without a newer modification time`, `previous build did not finish`. A reason says why a reset is due. The reset is carried out, and the reason reported, only when the cessation of the previous use's writers is established (D32); otherwise the run is refused. Whether an existing target has a previous use is decided by affirmative provenance, not by the absence of a known gate use (D33; sixth amendment, which withdraws the earlier wording "whose content no gate use is known to have produced"). A target that is affirmatively established never to have been used by a gate, and that holds a closed controlled plant (TV13), has no previous use: D14 and D15 apply to it as before, and `reset: no record` is reported as before. A target whose last gate use is closed by established cessation is a separate case, under D32; no marker, token or record establishes that cessation by itself. A target that exists and whose history is missing, unreadable, corrupt, incomplete or otherwise unresolved is refused, warm or cold: `no record` and `record unreadable` say why a reset would be due and are not provenance. The absence of a marker, a directory that was absent or recreated, the cooperative lease, a missing process id and an operator's acknowledgement establish no provenance. Until the durable provenance of P5 exists and is reviewed, never-used status cannot be established, so a `target/gate` that exists at the start of `full` is refused and TV13's deletion is unmet (section 12). A target that does not exist is created and reported as before; its absence is not provenance. `previous build did not finish` is therefore reported only after that cessation was established, never for a marker alone (D35). The wording of the refusal for unresolved writers is not fixed here; the frozen C tests match `warm target in use`, a reason and the writer's process id for the cases they have (section 12, RS1 and RS2), and any further fixed substring is added with C's next owned edit.

Kept target (sixth amendment; D34). A run that ends with the cessation of its writers unresolved writes `target: kept (<reason>)` and `qualifying: no: <reasons>`, where the reasons include the unresolved cessation. Both are shown in the summary of every tier, `fast` included. Step verdicts that were observed may stand in the table; they never make the run qualifying. A later run that is refused because of that earlier use names the earlier run's identity and end time where validated diagnostics supply them, and says `unavailable` where they do not; it invents no identity, takes no marker as authority and does not count the disclosure as cessation. The value of `verdict:` and the exit status of a run that kept its target, the fixed text of `<reason>`, and whether the start-of-run value of `target:` is shown beside `kept` are not fixed here: they are task F's interface obligation, reviewed before C asserts them.

Step details that tests match by substring: `no test executed`, `not evaluated`, `inconsistent inventory`, `incomplete inventory`, `witness missing`, `witness mismatch`, `not available yet`, `not a gate`, `unchanged`, `partial`, `created`. A refusal prints `REFUSED:` and its reason; for D26 the reason starts with `inherited variable` and lists the names.

Inventory file: one line per test, tab-separated `kind`, `package`, `target`, `test`, `status`, sorted bytewise. `kind` is cargo's target kind (`lib`, `bin`, `test`, `example`, `bench`) or `doc`. `status` is `ok`, `failed` or `ignored`. For `gate inventory`, `--executables` takes the JSON lines that `cargo test --no-run --message-format=json` prints; the command exits 0 when the log is accepted and 1 with the reason otherwise.

Evidence directory: `result.txt`, `steps/` (one log per step), `inventory.tsv`, `environment.txt` (names only), `tools.txt`, and `MANIFEST`, which lists `<blob id>  <path>` for every other file, sorted by path. The digest a pull request states is the blob id of `MANIFEST`. Blob ids detect a file changed after the fact; like the rest of the evidence they do not resist a producer who rewrites it (ADR, Consequences).

Selection output is the JSON of section 5 with `schema` 1.

**C resources and helper exits.** The miniature tests of task C discover the already installed pinned toolchain and the native `rustup` from the invoking Cargo, tool-home and `PATH` locations, then provision private, credential-free Cargo and Rustup homes and proxy-first tool `PATH`s for their children. A missing exported `CARGO_HOME` or `RUSTUP_HOME` is resolved from the invoking tool location or the pre-sentinel `HOME` convention; an explicit resource override is authoritative and is never replaced by a fallback. No `GATE_CONTRACT_RESOURCES` variable is required, and the gate neither sets nor advertises one. The tests do not install anything, copy host configuration or credentials, extend the evidence allowlist, or bypass the empty-`RUSTUP_HOME` test (TV6) by calling the toolchain directly. A failure to discover or provision resources is reported as a failure, never skipped. Task C owns the fixture implementation and the default-workflow and TV6 tests; task F owns the propagation of D8. The seven names that `environment.txt` may record as set stay `HOME`, `PATH`, `CARGO_HOME`, `RUSTUP_HOME`, `RUSTUP_TOOLCHAIN`, `CARGO_TARGET_DIR` and `XTASK_GATE_WITNESS_DIR`; controls that a test applies to its own private children are not part of that record.

`gate evidence verify` exits 0 when it accepts the evidence, 1 with the reason when it rejects the evidence or the expected-commit inputs (corrupt, incomplete, unreadable or nonqualifying), and 2 on a command-line usage error. A nonqualifying export is a rejection (1) for the verifier; exit 3 belongs to a producer run or export that succeeded but does not qualify. Verification takes no lease and runs no tier. A diagnostic names the rejected file, field, step or predicate: a mutated step status names that step and `status`, a mutated step exit names that step and `exit`, and a moved head may be named `head-end` or `moved`. No other exact wording is fixed. This clarifies a point the earlier text left open (this section states exits only for `gate inventory`). F0's exit 2 `not available yet` is dispatch evidence at the seam, not the rejection oracle.

### 12. Staging amendments after the seam commit

This section records decisions taken after the design froze at v2 and F0 was committed. It does not remove or relax a requirement, a decision of the ADR or a vector; it adds stage and detail to TV61–TV64 and TV72, corrects two "Verify" cells, and says when each obligation is executed and in what order the host runs the work. Every row is an obligation. Nothing here says that a protection works, that a tier is valid evidence or that any timing is met; those are shown only by executed tests and real runs.

**F0, the state of the seam.** F0 is one local commit, `12c263bc8e43df6bdf05efa770876ccd6eabc199` (tree `07df11a462b7259eb7f23dc2225dff1d7a4ad847`), whose parent is the frozen v2 design `e96803e50c2815100e96ee55b439f392b0b5d2b4`. It adds three files and 353 lines: `xtask/src/main.rs` (+9), `xtask/src/gate.rs` (+178) and `xtask/src/gate_selection.rs` (+166). It is not in a pull request.

- Implemented: the `gate` subcommand with the argument shapes of section 1; the selector types and signatures of section 8 (`Change`, `Class`, `Mode`, `Format`, `SelectArgs`, `Reason`, `Selection`); `gate inventory --exit` as an `i32` that defaults to 0; the trailing `lease exec` argument vector, which is never started; no shared-lease flag.
- Behaviour: every tier, `--plan`, `gate inventory`, `gate evidence verify` and `gate lease …` print `not available yet` and exit 2. `gate select` prints the schema-1 result with mode `all`, no packages and one `error` reason, and reads no fixture file. `--help` exits 0 and says which tier is the merge condition and which is the default.
- Targeted verification, run by F0's producer and retained, not repeated by anyone else: `cargo test -p xtask` (64 unit and 28 `client_boundary` tests passed, none failed or ignored), `cargo xtask crate-graph`, `cargo fmt --all --check`, and 18 checks of the built binary's exit codes. A reviewer then compared the committed bytes with the handoff statically.
- Not run: clippy, any gate, any CI run, the full workspace suite, a measurement. F0 is not F and certifies nothing.
- Synthetic fixture for the evidence tests: the F0 handoff supplies a field grammar and a recipe that builds a complete, explicitly synthetic evidence bundle and a miniature repository. It is a test input. It is never evidence of a real qualifying run and never replaces the real run of PR 1.

**Where the vectors stand against F0.** Against F0, every test that expects success is red because the command answers exit 2, and the selector's per-path reasons are red because the stub gives one `error` reason. That red result shows the tests compile and the seam answers; it does not show that any planted negative reaches its own oracle. Two results are therefore recorded separately: the F0 red result, and, once F exists, the exact planted-failure reason of each negative case together with its matching positive control. A generic non-zero exit never counts as protection evidence. A positive control must return 0 before its negatives count (TV40; TV47's matching case). No test is ignored or weakened to turn it green.

**Staged to PR 2: TV61, TV62 and the shared-fast case of RS1.**

- Why: in PR 1 `fast` is unavailable (section 2), `gate lease exec` takes the exclusive lease, and no shared acquisition command is specified. A fixture that holds a shared kernel lock itself would test the kernel, not the gate's admission and fairness. No `--shared` flag is added to fill the gap.
- Required in PR 2, with `fast` as the real shared requester: TV61 (two shared requesters both acquire), TV62 (a shared holder, a queued exclusive waiter and a new shared requester, each observed through a handshake; the newcomer runs only after the exclusive waiter completes) and RS1's concurrent warm-target case with two `fast` runs. A sleep, a fixture's own shared lock, an ignored test or a waiver does not satisfy them. PR 2 is not accepted without them.
- Required in PR 1 regardless: the warm and cold target is protected by a single writer, tested through the entries PR 1 has (exclusive and inherited holders). The test must make two requests contend on the same target in a way the host lease does not serialize; two requests that the host lease alone orders prove nothing about the target lock.
- This is a staging decision, not a waiver. The frozen spec and ADR are not rewritten; where their wording lets TV61 or TV62 read as PR 1 work, this section governs the order.
- The gate's TV63 (the holder is killed and the lease is free at once) and a measurement supervisor's guardian that keeps a lease through its own teardown are different contracts. Neither stands in for the other.

**Obligations of PR 1, from the resilience, security and release readiness checks.** Test files are those of task C; the implementation is task F's. Each case uses a disposable miniature workspace, a private home and lease directory, fake commands and short bounds, and never the real home, the host lease or a real agent CLI.

| Id | Obligation | Required test |
|---|---|---|
| RS1 | A warm target has one writer. A second run waits or refuses (`warm target in use`) and never resets the target under the first. The per-target lock lives outside `target/gate-warm` and is held from marker to record. | Two runs contend on one target (PR 1: through the available entries; the shared-fast case is PR 2, above) |
| RS2 | A hard-killed gate may leave build processes writing into its target. The next run does not delete or reuse a warm or cold target while the previous process tree lives; it refuses and names the process ids (sending no signal) or waits for a bounded time. Fifth amendment (K-3, D34): a wait that ends without independently established cessation ends in the same refusal, with the target unchanged; the recorded process ids being gone after the wait is not enough. Sixth amendment (D33, D36): the next run does not need to know of that tree to refuse. An existing target whose history is unknown is refused, and for the cold target that is every `target/gate` present at the start of `full` until P5 exists (RC11). | SIGKILL the gate during a warm build and during a `full` build with a live child, then start the next run |
| RS3 | The warm record is written to a temporary file and renamed, before the marker is removed. A truncated record reads as `record unreadable`; a file missing from the record counts as changed. Sixth amendment (D33): `record unreadable` is the reason a reset would be due; the reset is carried out only on affirmative provenance, and a target with an unreadable, truncated or incomplete record is refused without it (RC11). | A fault after the record and before the marker is removed; a truncated record; a missing entry |
| RS5, INV-T | Every wait the gate performs is bounded: the lease wait, the wait for children after a signal, and the wait for the build tree to finish. On INT, TERM or HUP the whole attributable descendant set is stopped within a bounded grace period, including `setsid` and `process_group(0)` members and a child that ignores TERM (escalated to KILL). The result is `INTERRUPTED`, listing survivors, never `PASSED`; no warm record is written while a build process survives; the lease is released. Fifth amendment (D32, D35, D36, D38): no warm record is written and no target is removed unless cessation is independently established, which is more than no known build process surviving; the grace period has no number yet. Sixth amendment (D38): the bounds of this row and of D34 are listed under P4, and none of them has a value. | TV64 plus a TERM-ignoring child and a `setsid` grandchild |
| RS6 | TV63 is meaningful only if the holder has live descendants when it dies and they do not keep the lease. | TV63 with live descendants |
| RS7 | `lease: inherited` counts only while a live holder holds the exclusive lock and the token matches. A stale holder record, or a token carried by an orphaned child, grants nothing; the gate takes the lease normally. | `lease exec` killed while its child gate holds the token (also TV67) |
| RS4, RS8, RS10 | Retained follow-ups, not part of PR 1 acceptance: an edit during the build that is reverted with an old modification time resets the next run (RS4); `lease exec` with a detached descendant delays or reports release (RS8); an orphaned test binary counts as foreign build activity (RS10). | Named when taken up |
| RS9 | Verified on each real host before anything relies on the lease: on Linux two sessions resolve the same device and inode for the lease file (no per-service private `/tmp`); on macOS the `/tmp` symlink's parent is not refused while a symbolic link as the lock file still is (TV66). | A check on each host |
| ENV-REFUSE | TV17 extended: each old-reader opt-in alone and together, the blessing, stress and index knobs, an unknown name and an empty value refuse before the lease and the suite, and a secret canary never appears in output or evidence; all session names together proceed; an invalid-UTF-8 name is refused where the OS allows it. | TV17 and extensions |
| JUDGE-PATHS | The judge identity is the same from working files and from the commit, for names with spaces, tabs, newlines, quotes, backslashes, a leading dash and non-UTF-8 bytes; added, deleted, renamed, tracked-but-ignored and untracked inputs change it; a hashing or enumeration failure never gives a trusted stamp. No path reaches a shell. | TV19 and TV47 extended |
| JUDGE-OWNERSHIP, BOOT-OWNER | Symbolic links at `target`, `target/gate-xtask` or the stamp, special files and an inherited target are refused before any deletion or build, with an outside canary untouched; two bootstraps in one worktree do not interleave deletion and execution. | TV19 extended |
| BOOT-PATH, BOOT-INT, BOOT-RACE | The hostile-name identity above; a build that fails, receives INT or TERM, or is killed before the stamp is published leaves no trusted stamp; a source change during the build cannot publish a stamp for different bytes. | Named cases of TV19 |
| EVIDENCE-INTEGRITY | Changed exported bytes, an absent or wrong judge identity, malformed, duplicate or traversing manifest paths, symbolic-link entries, and missing or extra files are rejected before any entry is followed. | TV41, TV47 and extensions |
| EVIDENCE-REDACTION | Canaries for a password, a prompt, a token and path strings (also in escaped forms) are absent from the exported copy, which is redacted before it is hashed. | TV17, TV46 |
| LEASE-FILES | The no-follow, mode and ownership checks apply to the lock, the turnstile and the holder record. A foreign-owned file is a refusal, never unlinked and never replaced by another lease. TV66's foreign-owner case needs a disposable ownership fixture or a reviewed boundary seam; an unprivileged test cannot create one, and a permission error is not a refusal. It is not silently skipped. | TV60, TV63, TV65–TV67 |

The ordering these obligations imply for the warm target (D30 as amended by D35): take the per-target lock; write the marker; capture the identities; build; establish, within a bounded wait, that every writer of this use has ceased (D33: by the observation of the reviewed boundary, not by a list of process ids, end of file or the wait itself); take the end time; write the record by temporary file and rename; remove the marker; release the lock. If cessation is not established when the wait ends, no record is written, the marker stays and the target is not touched again (D34). If the marker cannot be written, nothing builds. The earlier wording of this step, "wait, bounded, until no process of the build tree is alive", is replaced: a gate cannot conclude that from the processes it can see.

An internal suite deadline is optional. If one is added it can only fail the run (`FAIL: suite deadline exceeded`), never pass it and never mark it `ENV-LIMITED`, and it is a safety cap, not a timing waiver.

**Evidence-fixture rules.** TV41 corrupts a log and leaves the manifest stale on purpose. Every other semantic case (TV42–TV45, TV47) starts from a fresh copy of the positive, changes one condition, and regenerates every affected manifest entry and the manifest's digest, while the independently supplied expected commit, source identity and judge identity stay correct unless one of them is the subject of the case. Wrong digest or wrong commit cannot stand in for the judge case, and the reverse. Several rows of TV43–TV45 need one case per field. The `status` and `exit` fields of a step are changed separately when a case claims to test one of them. For inherited-lease cases the inherited positive is established first. TV46 uses an actual otherwise-successful partial miniature run, never the synthetic bundle.

**Inventory and ordering cases that are required in addition to TV7–TV11.**

- `merged_child_output_cannot_inflate_passing_inventory`: one real passing test, a child-written `test forged … ok` line outside a `failures:` block, summary `1 passed`, exit 0: rejected as `inconsistent inventory`; the clean control is accepted.
- `split_result_line_cannot_be_accepted_as_complete_inventory`: a real result line split by child bytes, summary intact: rejected as `inconsistent inventory`. A mapped executable without its completed summary is rejected separately as `incomplete inventory`. Fake lines inside a `failures:` block stay ignored (TV11).
- `selected_packages_build_current_cli_before_infra_path_exec` (D31 as amended by D40; corrected by the sixth amendment): a disposable two-package workspace named `orch-cli` and `orch-infra` with no dependency between them; the CLI has an `orch` binary and an integration-test target, and the infra test runs `target/debug/orch` and asserts a source marker. The evidence is the order of commands that the gate itself sets for a selection of both packages (D40, H2): run on an empty target and again after the marker changes, that sequence starts the current binary both times, on macOS and on native Linux. A pass of one hand-written cargo invocation naming both packages with `-p` does not show what the gate does and is not this obligation; the earlier wording that prescribed it is withdrawn. The selector table proves neither build order nor binary freshness. The real `concurrent_index` test is run in isolation later, against the integrated selection. Both are due; neither has run, and this amendment launches no probe.
- R4: every test whose outcome differs between the ordinary and the folded run is diagnosed by running it alone in both environments; failures are kept; a later green never replaces the first red.
- The aggregate cost of the new test files stays inside the budget of section 8 and is measured, not assumed.

**Daemon-harness test.** The intermittent `daemon_harness_hermetic` failure (F7) has a hypothesis, not a reproduction or a fix: a deterministic `request.cancel` fence while the held spawn stays parked, rather than an expectation of a successful response during teardown. A test-only repair is a recommended follow-up. It keeps the scratch-lifetime, cleanup and `StopIncomplete` assertions and the original failure evidence, and is not a speedup or a passing baseline.

**Order of work on the host.** One heavy build, test or probe per host at a time, so the pre-implementation wave is a pipeline whose ordering is a host-resource exclusion, not a data dependency.

1. Author, in parallel and in disjoint paths, each in its own worktree and branch from F0: C (the seven test files of section 8), G (`xtask/src/gate_selection.rs` and `xtask/src/gate_selection/**`), H (`xtask/tests/gate_selection.rs`) and this plan. Authors use static checks only: no cargo, build, test or probe. Their handoffs are provisional and are not executable readiness.
2. Validate, one after another: C, then G, then H. Each owner may repair only its own scope while validating, keeps every failed attempt and freezes its final commit. Every worktree has its own target; none is shared. Validation is bounded, targeted work, not a measurement. If a fixture cannot be run without an uncontained process tree, validation stops and says so.
3. Review frozen commits: C's code review follows C's validation and may overlap G's and H's validation; the G and H review follows both and includes this plan. Reviewers run no builds or probes in this wave; they read the retained execution evidence and name any further check that is needed.
4. F-SIGNAL-SEAM runs after H's validation. Security and resilience then review the frozen C, G and H sources and the probe's bytes and evidence. A checkpoint reads the actual verdicts, commit identities and validation records, not task statuses, and then schedules only concrete fixes or rechecks. An edit after validation is a new identity and gets a delta review.

**Held work.** The following are not ready, and listing them is not a promise of when they will be.

- **D, the measured baseline (task D).** It waits for a measurement-only supervisor that has been reviewed independently: a lease bridge on the gate's lock path, containment that also reaches detached and re-parented processes (unknown attribution is an incomplete teardown and no measurement, and no unrelated process is ever signalled), a termination ladder, loss of the supervisor, resource stops, a telemetry list declared before measuring (thermal pressure is required, and no required item is relaxed after a failure), durable logs, and a dry run on each platform used. The first candidate was reviewed by release engineering, security and resilience and was blocked by each; its ownership, lease and containment defects are open. No baseline measurement, full suite, gate or benchmark has run. Native Linux access, durable evidence transfer and routing are unassigned. An ordinary CI result does not stand in for native macOS acceptance; the report names which platform did not run.
- **F, the folded gate.** It waits for C's executable red evidence in both phases above, the actual baseline of D, the applicable challenge dispositions and F-SIGNAL-SEAM.
- **F-SIGNAL-SEAM.** Rust's `std` has no safe signal-handler registration, so INT and TERM handling, a lease that outlives its direct holder until teardown, and stopping the whole owned tree are not delivered by Drop guards or `Child::kill`. A no-manifest mechanism is acceptable only if a bounded disposable probe, with private fixtures and independent cleanup, shows direct-holder INT and TERM, the lease's lifetime through teardown and teardown of the whole owned tree. Failing to show that is a valid block, not permission for a dependency or an unsafe workaround; a manifest or dependency exception needs an explicit, recorded scope decision. Section 8's statement that `xtask/Cargo.toml` does not change stands until such a decision exists.

**Amendments after the C repair and the G and H validation.** Like the rest of this section, every item is a stage record or an obligation. None says that a protection works, that a selector is accepted, that a tier is valid evidence or that any timing is met. No requirement, vector or decision of the ADR is removed or relaxed. No independent acceptance of C, G or H exists.

*Verification states.* Four results have been conflated in earlier wording and are kept apart from here on. A result of one state never stands in for another.

| State | What it is | Where it stands |
|---|---|---|
| F0 unavailable | A test run against the seam: every tier answers exit 2 `not available yet`, and the selector stub gives one `error` reason. It shows that the tests compile and dispatch, and nothing about any protection | Task C as first frozen (`f057a481a9d442e0189b33a099fa97e1a9c2d411`): a staged sweep of 93 executions, 15 pass and 78 fail, as recorded by its producer. Task H (`6c518360cb09dbc29751c3a3c269a2c148d183c7`): 34 outer tests, 1 pass and 33 fail. Retained; not repeated for the repaired C |
| Fixture control | A control of a test's own helper (raw output capture, Cargo argument recognition, installed-tool provisioning, the witness test) or a mutant of that helper | Repaired C (`412523136b3e5ab4b8efdb7146747637f28ae5fe`, tree `94bde8f0913ff16b9e053a1f8e035c7031797789`), on macOS only: 35 passes (five fixture behaviours, compiled into each of the seven test binaries); one default-provisioning control with `CARGO_HOME` and `RUSTUP_HOME` removed, passing; one explicit empty `RUSTUP_HOME` control, failing by design with no fallback; three fixture mutants, each failing as intended. These show the helpers behave. None shows a gate protection |
| Held scenario | A scenario that is not launched, or not credited, because the boundary needed to run or observe it safely does not exist | The five lifecycle scenarios of task C; the TV68 sampler cases; the proof that a manifest path is not opened before it is validated; every native Linux run; the Homebrew wrapper, `HOME/.rustup` and `CARGO_HOME/bin/rustup` branches of the provisioning discovery and the folded (D8) invocation of it |
| Implemented-F positive and mutation | A matching positive that returns 0, then one planted failure that fails for its own stated reason, against the implemented gate | None exists, because F is held. Every protection of sections 3 to 7 still needs it |

The repaired C has 119 tests by enumeration on macOS (15 bootstrap, 24 evidence, 15 inventory, 18 lease, 28 protections, 14 tiers, 5 witness); that is a list, not an execution.

*Status of C, G and H.* This replaces the earlier "authored, not executed" wording.

- **C.** The repaired commit above changes only the seven files of task C (+878/−234). The original reviewer's static delta review found the source repairs for the will-break and false-green defects in place: the mandatory resource variable is gone, the false green of the boot-interrupt case is fixed, the evidence-path, size and NUL vectors are isolated for regular files, and the attribution requirements are tightened. Open after that review, and not closed by it: the stop observation and survivor authority of INV-T (held); the open-before-validate proof; the Mac layouts and native Linux runs named above; the new target-path defect below; and TV68. The security and resilience delta reviews of the repaired commit have since reported; their dispositions are in the third amendment below. C is not accepted.
- **G** (`fddb1281e1b51ccfca721c84d6fe1edcd6ffff0f`, tree `da141cd84332915385e15dbb7873e2f6947072ad`, five selector files). Its producer's targeted run: 17 inline tests pass; with the selector body reverted to the F0 stub the same tests compile and 6 fail; 70 child commands (57 selector invocations, 54 exiting 0 and 3 exiting 2 as designed) met their expected assertions; `clippy`, `rustfmt` and `crate-graph` on its scope passed. These are runtime assertions of a producer, not R5 or R6 acceptance.
- **H** (`6c518360cb09dbc29751c3a3c269a2c148d183c7`, tree `8bf9c0cf67eea19e8c5db9014730c245e15e8732`, one file, 34 outer tests). Against F0, 1 passes (the half-pair usage case) and 33 fail: 19 on the mode, 6 on a missing path or class, 1 on the GitHub output, 7 because the stub's unavailable reason is rejected. H does not treat the stub's select-everything answer as a pass, so the earlier statement that the select-everything rows pass against the stub is wrong and is corrected in the task table. Loop cases stop at their first failed assertion; later sub-cases have not run.
- **Due for G and H, none run:** H against G (macOS and native Linux); a real-Cargo miniature with a member-to-member path dependency, which is the only way the path-dependency branch of the graph reader executes, and one real `gate select` on a single-crate diff of this repository; the two-marker `selected_packages_build_current_cli_before_infra_path_exec` probe on both platforms; the real `concurrent_index` test in isolation against the integrated selection; and a cleanup in H's fixtures that cannot panic, in H's next edit. Selection, review of the frozen commits and integration are still separate steps; none of the above is accepted.

*N1: the selected target is compared by identity, not by spelling.* Section 4 specifies `CARGO_TARGET_DIR` as `target/gate-warm` or `target/gate`, relative, and an absolute spelling is as legitimate (on macOS a temporary directory may be reached through a symbolic link). The repaired C compares the exact string that the boundary reports with one canonical path, so a correct F that sets the relative value fails before any single-writer assertion. A correction is due in C before PR 1: resolve the reported value against the gate child's working directory, then compare filesystem identity or canonical paths, with an owned-directory positive control and a negative control. F is not required to emit an absolute canonical spelling. The corrected C returns to the original reviewer. The correction is in C `373ea896c3d6149fa5c9c43fc12f22228d57033c` and is accepted by a static reading only: its eight target controls are authored and have not been compiled or run, and the canonicalisation is a metadata-only observation (see the disposition below), not confinement before resolution. (That was the state at the fourth amendment. The eight controls have run since; see the fifth amendment below.)

*TV68 and the `ps` and load observations.* Plan §6 names the commands and observations of the sampler. It does not define an interception interface: an implementation may call absolute tools, and C may not assume that a `PATH` stub is taken. The TV68 cases that inject a foreign `cargo` process or an unavailable load reading through `PATH` stubs are therefore not contractual. Comments in the test source that call them conditional do not stop them from running in every default `cargo test`; they are unmet, not skipped. Before PR 1, the owners of C and F replace the assumption with behaviour that the contract fixes, internal fault tests of F's parser and sampler (including Git error propagation, `ps` output and error parsing, and the load reading on each OS: `sysctl` failure on macOS, `/proc/loadavg` on Linux), plus a native observation, and keep the independent negative controls. No ignored test, availability-only pass, public hook or allowlist extension is accepted in their place.

*Signal, lifecycle and cleanup: the hold stands.* F-SIGNAL-SEAM and the five lifecycle scenarios stay held for these reasons, which no change so far removes.

- Ownership and signal authority (review id FS1) have not been established: a descendant can fork, create a new session and be re-parented between observations, and a process id seen once is not an identity that a later signal may rely on. Unknown ownership cannot authorize a signal, a removal, reset or reuse of a target, a completed D30 record, or a claim of whole-tree teardown.
- End of file on a fixture's channel is not death and not cleanup authority (review id FS2). A fixture needs an independently reviewed lifecycle and cleanup boundary and stronger survivor observations before the five scenarios launch. Self-bounded workers and end of file may help a diagnosis; they do not certify INV-T, TV63 or TV64.
- RS5 and INV-T stay as written in the table above.
- Disclosing a residual process and resetting the same target (the resilience reviewer's proposal, SR-4) is not adopted as closure. A disclosure does not stop a writer, and resetting a target does not stop that writer from writing into the reset tree. This is a hold on operations that cannot be proved safe, not a new fencing scheme. A viable boundary and its acceptance by the specialist reviewers are still required. No dedicated account, privilege, launchd job, kernel containment or speculative replacement is authorized here.
- Three things are different and none stands in for another. TV63 (RS6): the actual sole holder of the gate's lease is killed with SIGKILL while it has live descendants, and the kernel releases the lease at once. INV-T and TV64 (RS5): a handled INT, TERM or HUP meets the bounded teardown and publication contract. The measurement guardian of the held baseline task (D) holds a lease through its own teardown, which is another contract. A helper must not quietly change which process is the holder, and the guardian's retention substitutes for neither. The runtime owner and topology are unresolved and held. This plan invents no runtime.

*D31, broader coupling (pending, PR 2).* The selector as frozen at G adds `orch-infra` only when `orch-cli` itself changed. A change in a crate that `orch-cli` depends on (for example the daemon, the engine, the client, the protocol or the terminal interface) selects `orch-cli` through the normal closure and does not select `orch-infra`, so `fast` would skip `concurrent_index`, whose subject is the daemon behind the `orch` binary it starts. The lead's decision is the broader intent: after the normal closure, if the selection contains `orch-cli` and not `orch-infra`, add `orch-infra` with a `coupled` reason and do not add its dependents. The case where `orch-infra` itself changes (its normal closure, with no `coupled` reason) and the exclusion of the extra dependent sentinel stay. TV38 gains a case for a crate that `orch-cli` depends on and `orch-infra` does not: `orch-cli` and `orch-infra` are selected, the second with a `coupled` reason. This is a decision about a correction that is still to be written and tested; neither G nor H implements or tests it now, and nothing here says the proposed change works. ADR 0013 decision 31 is narrower than this intent. Its text is not rewritten by this plan. A properly owned successor or amendment ADR must reconcile the two before or together with the implementation. The code change, the TV38 extension and the real-Cargo miniature are PR 2 obligations. (That record now exists as ADR 0014 D39 and D40, proposed; see the fifth amendment below.)

*Third amendment: the delta reviews of the repaired C, the recovery holds and the holder.* Like the rest of this section, every item is a stage record, a hold or an obligation. None is an implemented recovery, none changes an accepted decision of the ADR, a requirement or a vector, and none says that a protection works. No runtime, recovery mechanism or account is chosen here.

*Review dispositions at the repaired commit (`412523136b3e5ab4b8efdb7146747637f28ae5fe`).*

- **Security.** The hardening of the installed-tool discovery and of the fixture helpers is accepted as a static reading of the scoped delta only; no new exploitable defect was found in it. The block on FS2 remains: end of file on a channel or the direct child's reaping is not proof that its descendants have ceased, and the safe removal of scratch after a normal finish is unproved. FS1 and the earlier security findings S1–S5 also stand.
- **Resilience.** SR-1 (an expired worker no longer passes TV64), SR-3 (the gate-tier lease descriptor and a specific refusal) and SR-6 (nested runs are cleaned up through their own, unreaped handle) are met by a static reading of the source only. The lifecycle scenarios did not run, so none of the three has runtime credit. SR-2 (publication ordering), SR-4, SR-5 (end of file taken as exit before recovery), SR-7 (teardown independent of logging) and SR-8 stay held or unmet.
- **Source corrections in C `373ea896c3d6149fa5c9c43fc12f22228d57033c` (tree `c0345bb568699b62cdc513e04b5812da0a022faf`, parent `412523136b3e5ab4b8efdb7146747637f28ae5fe`): accepted by a static reading only.** The commit changes `xtask/tests/gate_lease.rs` and `xtask/tests/gate_witness.rs` and nothing else. The original code, security and resilience reviewers each read it; none ran anything. The eight selected-target controls (`gate_lease`) and the three scratch-retention controls (`gate_witness`) are authored; compilation and the discriminating mutation of each are not done as of this amendment, so nothing here is runtime evidence.
  - N1 (above): the selected target is compared by identity, not by spelling. Security accepts the canonicalisation as metadata-only: it can traverse external metadata before the final equality check and gives no content-read, enumeration, creation, mutation or cleanup authority over a reported path. It is not confinement before resolution, and no protection against a race by the same account is claimed.
  - SR-10: the resistant fixture worker no longer cleans up its detached leaf on an interrupt, so the TV64 interrupt rows cannot pass unless F stops a detached descendant, as the TERM and HUP rows already require. The discrimination can be shown only by the held lifecycle scenario. The filesystem controls cannot exercise it, so it has no safe executable control before the lifecycle seam exists.
  - G-1: the fixture retains its scratch while a test is unwinding or when an unresolved-child marker exists, and says so on standard error. Retention preserves evidence only. After a normal finish the scratch is still deleted without independent observation that any writer has ceased, which is FS2 and stays a block. It does not show that any writer is dead.

*Recovery holds K-1 to K-4.* These are holds on operations whose safety is not proved. They do not adopt a recovery mechanism, and they are not rewrites of D30.

| Id | Hold |
|---|---|
| K-1 | A handled interrupt alone does not authorize a completed D30 record, reuse of the target or removal of the cold target (see the cold-target paragraph below). Reuse stays conditional on independently established cessation of the writers and on the existing source-identity checks. TV64's expectation of `target: reused` on the `pr` tier cannot count as acceptance without that prerequisite. It is not weakened into a claim about the attributable set only, and it is not rewritten here. Resolving it operationally and reconciling D30 is a blocker before PR 1 |
| K-2 | A surviving marker, or the absence of the recorded process id, does not authorize resetting, removing or reusing the same target after the holder has died. No automatic recovery is released while ownership is unknown. A fresh-path quarantine, an operator acknowledgement, a dedicated account or any other recovery is neither adopted nor ruled out. The automatic-reset text of D30 and the recovery test (`reset: previous build did not finish`) need an owned reconciliation before implementation. (Corrected by the fifth amendment: an operator's acknowledgement alone does not establish cessation and authorizes nothing, D33; the reconciliation is D34 and D35) |
| K-3 | A bounded wait is not by itself acceptance. When the wait expires with live or unresolved writers, the run refuses and does not change the target. The absence of a recorded process id after the wait is not enough; authority that the writers have ceased remains the prerequisite. C's deliberately failing branch for the missing observation stays until a reviewed interface supplies it |
| K-4 | The hold covers normal completion where a surviving writer remains unresolved. A blanket exemption for a normal finish is rejected, because it would contradict the block on FS2. This does not say that every run under a shared account is impossible, and it does not require certifying arbitrary foreign activity. It restates that direct reaping or end of file is not proof of descendant cessation for the owned-work scenarios. F's boundary and D30's normal-completion behaviour are unproved |

*K-5, the holder of the lease.* The holder of the gate lease is the process the caller starts, which may have been replaced by `exec`, and it is the sole owner of the lease's file descriptor. No helper silently keeps that descriptor after the actual holder is killed with SIGKILL. A helper that outlives the holder must not change the target: the next run may already hold the lease. A signal from such a helper needs authority from a handle it has itself not reaped. TV63, the graceful INV-T teardown and a measurement guardian remain three distinct contracts.

*Cold-target removal under unresolved writers (TV64).* The removal of the cold target `target/gate` after a handled interrupt is a mutation of the target, like a warm reset, reuse or publication, and it falls under K-1 to K-4 in the same way. It is not authorized by any of: an interrupt that was handled, end of file on a channel, the absence of a recorded process id, the direct reaping of the child, a surviving or absent marker, or the arrival of a bounded wait. Each of those is evidence at most; none establishes that the writers have ceased. Authority for the removal is the independently established cessation of every writer of the target, by a means the reviewed contract names (K-3, K-4 and FS2). Where that is not established, the run refuses and does not remove, reset, reuse or publish the target, and says why. Leaving the cold target in place in that case does not break this hold; a cold target removed under a live writer, which the writer may then recreate so that the next cold run begins on a tree it believes is fresh, would break the rule that nothing passes on a stale artifact. This paragraph restates the hold. It does not choose how cessation is established, does not change the C oracle for TV64, and does not rewrite D30; the proper reconciliation of D30 by a successor or amendment decision (K-1 to K-4, the architect as named owner) includes this case. (It is D36 of ADR 0014, proposed.)

*RS5, the numeric grace, is unmet.* The plan says only "a bounded grace period", and no number is chosen here. The constants in C are fixture budgets, not a product limit, and no value is taken from them. Before PR 1 the future original owner of F and the release (see below) supplies a concrete supported value, the reviewers assess it, and C's observation and self-expiry budgets are derived from it, not the reverse. Every bound is an idle or per-stage bound. None is, or implies, a limit on the total lifetime of a worker or a run.

*Retained scratch.* Scratch directories that match `/tmp/gate-contract-*` are kept evidence; nothing removes them automatically. A future runbook must describe them and give cleanup guidance only after cessation of every writer has been independently established. This plan gives no cleanup command, and none was run.

*Future owners.* The architect is the named future owner of the reconciliation of D30 and D31 with ADR 0013, by a successor or amendment decision. The original owner of F and the release is the named owner of runtime feasibility and of the numeric RS5 grace. These are named responsibilities. No task was dispatched for either, and no feasibility is claimed. (The reconciliation has since been written as ADR 0014, proposed; the runtime feasibility and the numeric grace are still open. See the fifth amendment below.)

*Unchanged and unresolved.*

- C-HELP: F0's `--help` text still omits the merge condition (TV70). C-USAGE: `pr --warm` is answered with exit 2 `not available yet` and does not name `--warm` (TV72). Both are defects of the production code, owned by task F. The C tests that show them stay red until F; none is weakened.
- The intermittent `daemon_harness_hermetic` failure (F7) is unresolved. The retained triage contrasts an assertion that accepts only success with a legitimate diagnostic cancellation; the exact historical interleaving is not proved. No fix, new reproduction, load attribution or speedup is claimed.
- D (the measured baseline) stays blocked and F stays held: no measurement-only supervisor has passed review, no bridge is reused, and the actual baseline, the folded gate and the signal seam are not ready. No acceptance, timing or equivalence claim is made.
- `check` and `test` both stay required until the owner applies, and the lead verifies, the reviewed main-only change. The owner merges every pull request.

*Fifth amendment: D30 and D31 reconciled with the rulings (ADR 0014, proposed).* The holds above stopped the builders from implementing what the lead had ruled out, while section 4, section 6 and ADR 0013 still prescribed it. This amendment removes that contradiction. It carries rulings that were already made; it adds no mechanism, chooses no runtime and relaxes no requirement, vector or lifecycle case. Nothing was built, run, probed or measured for it. ADR 0014 is proposed: it awaits the independent security and resilience reviews of its exact commit and the lead's decision. Until then K-1 to K-5 and the broader coupling keep binding as the lead's rulings, and a rejection of ADR 0014 would not release them.

| Ruling | ADR 0014 | Operative text corrected |
|---|---|---|
| K-1: a handled interrupt authorizes no completed record, no reuse and no cold removal | D32, D35, D36 | sections 4 and 6; I5; TV64 |
| K-2: a marker or a missing process id authorizes no reset after the holder died; an operator's acknowledgement is not cessation | D33, D34, D35 | section 4; the reset reasons of section 11; TV15 |
| K-3: a bounded wait that ends without proof refuses and changes nothing | D34 | RS2, RS5 and the ordering paragraph above |
| K-4: no exemption for a normal finish; a failed compilation is covered as well | D32 | section 4; RC1–RC4 below |
| K-5: the holder is the process the caller started; no helper keeps the lease | D37 | section 6; TV63 |
| Cold-target removal is a change of the target (fourth amendment) | D36 | sections 4 and 6; I5; TV64 |
| The grace period of RS5 has no number | D38 | section 6; RS5 |
| Broader coupling: a selected `orch-cli` selects `orch-infra` | D39 | section 5; TV38 |
| The binary a test starts by path is built from the same tree first, in an order the gate sets | D40 | section 5 |

*Why recovery stays refused.* A wait can show that time passed and that the processes the gate could see are gone. A descendant that was re-parented is not among them, so the end of the wait leaves the question open, and an open question is treated as a live writer (D34). If the gate reset or reused the target then, a late write would land in a tree that the next record describes as clean. The outcome of an exhausted wait is therefore the refusal, and it is still the refusal on the next run and the one after: elapsed time, a process id that is gone, the state of the marker and a person's acknowledgement add no observation of the writers. The same reasoning holds at a normal finish. A passing suite says that the tests passed. It does not say that every process a test or a build script started has exited. So the gate publishes no record and removes no cold target at a normal finish either, unless cessation is established. This does not say that every run under a shared account leaves writers behind, and it does not ask the gate to certify other work of the account. It says that the gate may not assume that its own descendants are gone.

*Recovery cases RC1–RC10.* They add detail to TV13–TV15, TV63, TV64, RS2 and RS5. No vector number changes and no existing case is removed. "Established" means by the observation of D33; the other column entries are what happened, not proof. In every refused case the target's content and the marker are unchanged afterwards and no signal went to a process the gate could not attribute.

| Id | The earlier use ended by | Cessation of its writers | Must |
|---|---|---|---|
| RC1 | a normal finish | established | Warm: the record is published with the end time taken at that moment and the marker is removed; the next run on unchanged source reports `reused`. `full`: `target/gate` is removed |
| RC2 | a normal finish | not established: a descendant is unresolved | No record; the marker stays; `target/gate` is left; the result shows `target: kept (<reason>)` and `qualifying: no` with that reason, in every tier. The next run on that target is refused, and so is each later one, until cessation is established (then RC8) |
| RC3 | a failed compilation | established | The record is published; the next run on the same source reports `reused` |
| RC4 | a failed compilation | not established | As RC2 |
| RC5 | a handled INT, TERM or HUP, each | established within the grace period | `INTERRUPTED`; the lease is released; the warm record is published, or `target/gate` is removed; the next `pr` run reports `reused` |
| RC6 | a handled INT, TERM or HUP, each | not established: a TERM-resistant or detached descendant is unresolved when the wait ends | `INTERRUPTED`, the survivors and the unattributed listed; the lease is released; no record; `target/gate` is left; the result shows `target: kept (<reason>)` and `qualifying: no`; the next run is refused, and so is each later one, until cessation is established (then RC8) |
| RC7 | SIGKILL of the holder, with live descendants | not established | The lease is free at once (TV63). The next run is refused with a reason, not with exit 75; it names what it observed, and the earlier run's identity and end time where validated diagnostics supply them, otherwise `unavailable`. Each later run is refused until cessation is established (then RC8) |
| RC8 | any ending without established cessation, and so without a completed record: RC2, RC4, RC6 or RC7 | established afterwards, by D33's means | Warm: the next run resets, with `reset: previous build did not finish`. Cold: the next `full` run removes `target/gate` and builds from nothing. Never `reused`. Until then every later run is refused (RC9, RC10) |
| RC9 | any of the above, cessation not established, and then each of these alone: the bounded wait runs out; the recorded process id is no longer present; every channel is at end of file; the direct child was reaped; an operator acknowledges | still not established | Refused each time |
| RC10 | as RC9, and the refused run is started again, several times and after a delay | still not established | Refused each time; a refusal never decays into a reset or a reuse |
| RC11 | (no ending is known) an existing target whose history is unknown, each alone: a warm target with neither marker nor record; a record that is unreadable, truncated or corrupt; a marker or record that is incomplete; a `target/gate` present at the start of `full` with no reviewed provenance, after `--keep`, after a killed run or of unknown origin | cannot be known | Refused at the start (`REFUSED`, exit 1). Nothing is deleted, reset, reused, built or signalled. Not `reset: no record`, not `reset: record unreadable`, not `cold` |
| RC12 | (no gate use) an existing target affirmatively established never to have been used by a gate: the closed controlled plant of TV13 | no previous use | D14 and D15 as before: the cold plant is deleted and `full` fails on the failing test (TV13); a warm one reports `reset: no record`. Unmet until the provenance of P5 exists; not passing and not waived |
| RC13 | as RC11, and then each of these alone: the directory was absent earlier or was recreated; no marker is present; the lease is free; a recorded process id is no longer present; an operator acknowledges | still unknown | Refused each time |

The cases with "established" cannot be executed before a reviewed observation exists (prerequisite P1 below), so they are held with the other lifecycle scenarios. They stay required. A gate that refuses in every case meets RC2, RC4, RC6, RC7, RC9 and RC10 and fails RC1, RC3, RC5 and RC8, and with them TV14, TV15, TV64, R3 and R12: PR 1 is not accepted on refusals alone. Sixth amendment: RC8 is generalized and RC11–RC13 are added by it; RC1, RC3 and RC5, where cessation is established before the record is completed, are unchanged. A gate that refuses in every case also meets RC11 and RC13 and fails RC12, and with it TV13. RC2, RC4 and RC6 hold in `fast`, `pr` and `full` alike: a run that kept its target qualifies in none of them. No vector number changes and no existing case is removed.

*Edits the frozen sources will need.* They are listed so that the reviewers and the owners can check them against the decisions. None is made here and none is authorized here: C stays frozen at `373ea896c3d6149fa5c9c43fc12f22228d57033c`, G at `fddb1281e1b51ccfca721c84d6fe1edcd6ffff0f` and H at `6c518360cb09dbc29751c3a3c269a2c148d183c7`. Each edit gives its file a new identity and a delta review by the original reviewers. No expected protection is removed by any of them.

- C1, `gate_lease.rs`, `direct_holder_int_term_hup_stop_term_resistant_and_detached_descendants` (TV64). The `full` rows assert that `target/gate` is gone after `INTERRUPTED`, and the `pr` rows assert `target: reused` on the next run. The test's own observation before those assertions is end of file on two channels, which is not cessation. The positive expectation (RC5) must rest on the reviewed observation, and the negative (RC6) is missing.
- C2, `gate_lease.rs`, `unfinished_warm_build_resets_after_owned_orphan_writer_has_exited` (TV15, third case). It kills the holder, takes end of file and expects `reset: previous build did not finish`. The reset (RC8) needs established cessation; the refusal for a marker without it (RC7) and the cases of RC9 are missing.
- C3, `gate_lease.rs`, `warm_and_cold_orphan_writers_are_not_reset_under_after_direct_holder_kill` (RS2). Its branch for the bounded wait fails on purpose because the outcome was undefined. D34 defines it: a refusal with the target unchanged, also when the recorded process id is gone after the wait. The oracle for that branch can now be written. The cold tier's expected reason, `build`, is weak.
- C4, `gate_protections.rs`, `failed_compile_allows_recovery_and_reuse_of_unchanged_source`. Its expected values stand (RC3). Its premise, "no live writer", is assumed by the test and must come from the reviewed observation; the negative (RC4) is missing.
- C5, new cases for a normal finish (RC1 and RC2) and for RC10. None exists.
- C6, the budgets. C's 30-second deadlines are re-derived once every bound of P4 has a supported, reviewed value (D38): not from the grace period alone, and not before (sixth amendment).
- C7, `killed_holder_releases_kernel_lease_while_descendant_with_token_is_still_live` (TV63). No expected value changes; D37 is what it already checks from outside. A case that a helper keeping the descriptor fails the test is added if F introduces any helper.
- C8 (sixth amendment), new cases for a kept target, in the tier and evidence tests. A run that ends with unresolved cessation shows `target: kept (<reason>)` and `qualifying: no` with that reason in `fast`, `pr` and `full`; `--evidence` on it exits 3; `gate evidence verify` rejects a bundle whose result shows a kept target, whatever its `qualifying:` line says (the negative controls of the verifier); a later refusal names the earlier run's identity and end time from validated diagnostics or says `unavailable`. None exists. They wait for F's interface for the verdict and the fixed substrings (P7).
- C9 (sixth amendment), new cases for unknown history (RC11 and RC13) and for the never-used plant (RC12). None exists. TV13's existing test stays as it is; its positive depends on P5.
- G1, the selector: the package row fires on the selection after the normal closure (D39), not on a changed path of `orch-cli`.
- H1, `gate_selection.rs`: the two existing TV38 cases stay; the indirect case is added, with a member that `orch-cli` depends on and `orch-infra` does not, and with the sentinel that depends on `orch-infra` staying unselected.
- H2, `selected_packages_build_current_cli_before_infra_path_exec`: the probe runs the order that the gate actually sets (D40), on macOS and on native Linux. A pass of one hand-written cargo invocation would not show what the gate does.

*Runtime prerequisites of task F.* Named, with their owners; none is designed, chosen or started here.

- P1, the observation of D33: a reviewed way to know which processes belong to a use, including descendants that left the group, started a session or were re-parented, and to know that they have ceased; with authority to stop them that does not rest on a process id checked earlier. Owner: the original owner of F and the release. Acceptance: the security reviewer (FS1, FS2) and the resilience reviewer. None exists for a macOS host where all agents share one account.
- P2, the signal seam: handling of INT, TERM and HUP by the actual holder without a manifest change, or a recorded scope decision that permits one (F-SIGNAL-SEAM above).
- P3, a holder topology that meets D37 and TV63.
- P4, the numeric bounds (D38; rewritten by the sixth amendment). It is a set, not one number: (a) the grace period of a handled INT, TERM or HUP; (b) the wait for the gate's children after a signal (RS5); (c) the wait for cessation at the end of a run, before a publication or a removal (D34, D35; RS5's "wait for the build tree to finish"); (d) the wait for cessation at the start of a run that found an earlier use (RS2, D34). The wait for the lease has its own existing value (D21, `--lease-wait`) and is not in the set. The original owner of F and the release says whether (a) to (d) are distinct bounds or one shared bound, and supplies every supported value with its justification, once P1 has a workable route. The independent reviewers assess the whole set before any budget of C is derived from it (C6). No number is chosen here, and none of the four has a value.
- P5, durable provenance of a target (rewritten by the sixth amendment). It is load-bearing and it is not implemented. A later run must be able to know, affirmatively and durably, which case of D33 an existing target is in: no gate use ever began in it; a use began and reached established cessation; or a use began and did not. It goes with the atomic record of RS3 and the ordering above. The marker and the record of D30 and D35 are the designed trace for the warm target, and they cover it only while both are present, readable and complete. The cold target has no equivalent in ADR 0013 and needs one, so that the next run recognizes a killed or a kept `full` run (RS2) and can still delete a target that only a plain cargo command produced (TV13). Until P5 exists and is reviewed: a `target/gate` that exists at the start of `full` is refused; TV13's deletion is unmet; a warm target whose marker or record is missing, unreadable, corrupt or incomplete is refused. A token never establishes cessation; that stays the observation of P1. Whether a later run may rely on a record as read is part of P5's review, and the open security finding on recovery contents used as authority is not closed by this plan. A directory that was absent or recreated, a missing marker, the cooperative lease, a missing process id and an operator's acknowledgement establish no provenance. Owner: the original owner of F and the release. Acceptance: the security and resilience reviewers, on a frozen design and then on its implementation, before any runtime work that depends on it. No token, file or mechanism is chosen here. The gate cannot detect a marker or a history changed by hand; no document offers that as a recovery.
- P6, teardown that does not depend on logging, publication ordering observed with a live writer, and no recovery taken from end of file (resilience findings SR-7, SR-2 and SR-5).
- P7, the presentation of a run whose steps passed and whose cessation was not established (rewritten by the sixth amendment). The floor is decided by the lead and carried in D34: such a run does not qualify as builder, merge, goal or release evidence in any tier; its result carries `qualifying: no` with that reason and `target: kept (<reason>)`; step results that were observed may be shown and cannot make a qualifying overall success. Still open, as F's bounded interface obligation: the value of the overall verdict and the exit status of such a run, and the fixed substrings. The verifier's negative controls are due (C8).
- The earlier prerequisites are unchanged: the measured baseline of D, the TV68 observation seam, and the implemented-F positives and mutations for every protection.

*Current evidence for C (this replaces "unrun" for eleven controls only).* The eleven filesystem controls of C `373ea896c3d6149fa5c9c43fc12f22228d57033c` have been executed on native macOS, after the fourth amendment, and the lead accepted the result within its boundary. They are the eight selected-target controls of `gate_lease` and the three scratch-retention controls of `gate_witness`. They start no child process. Each was run by its exact name in three states: the original source, where it passed; its own single mutation, where it failed with exit 101 and its own expected error; and the restored source, where it passed again. That is 33 single-test runs on 12 builds made by calling the compiler directly, without cargo's test runner and without the default suite. The original security and release reviewers cleared the exact commands before they ran. The record is in the lead's goal record, not in this repository. What it shows: the target-identity helper (N1) and the scratch-retention helper (G-1) behave as their controls say. What it does not show: any behaviour of the gate, a clippy or CI result, a timing, or anything about a process. SR-10 has no executed control and stays a static reading. Every lifecycle scenario, every signal case and every case of RC1–RC10 is unrun. The normal-drop control deletes a tree that never had a writer; it does not close FS2. C is not accepted.

*What this amendment achieves, and its limit.* The documents no longer tell the builders to publish, reset, reuse or remove a target on evidence the lead has ruled insufficient, and the selector's intended coupling and build order are stated. That is a correction of the contract. It is not a workable gate. On a macOS host where all agents share one account there is no reviewed observation for D33 today (the feasibility check of the signal seam stopped on exactly that, and the first measurement supervisor was blocked for it). Without one, a gate that follows these decisions refuses every publication, reuse, reset and removal there, which fails the cases listed under RC above. F and D therefore stay blocked, and this plan does not present that state as passing. The one remaining prerequisite that unblocks the recovery contract is P1, accepted by the security and resilience reviewers; P2 to P7 follow it. (Corrected by the sixth amendment: P5 does not merely follow P1. It is load-bearing by itself for every later recovery and for every start-of-run deletion of an existing cold target; see below.) Whether native Linux offers such an observation has not been evaluated here. A fresh target path instead of a reset and a dedicated account for gate runs are neither adopted nor ruled out, and neither was evaluated.

*Sixth amendment: the fifth corrected after its delta reviews (ADR 0014 revision 2, still proposed).* The security and resilience reviewers read the fifth amendment at its exact commit. Each passed it with conditions, and the lead ruled on every point. This amendment makes those corrections and nothing else. It adds no mechanism, designs no provenance, chooses no number and relaxes no requirement, vector or lifecycle case. Nothing was built, run, probed, signalled or measured for it. It corrects documents; it clears no runtime hold.

| Finding (review id) | Correction | Where |
|---|---|---|
| CD1 (security), R-1 (resilience): "no gate use is known" classified an unresolved target as a plain-cargo plant | Affirmative provenance. Three cases: never used by a gate (established), a previous use closed by established cessation (a separate case; no token proves it), and unknown history, which is refused for the warm and the cold target | ADR 0014 D33, D34, D36, A31; section 4; the reset reasons of section 11; RS2, RS3; RC11–RC13; C9; the TV13 note |
| R-1, the dependency on P5 | P5 is load-bearing and unimplemented. Until it exists, an existing cold target is refused at the start of `full` and TV13's deletion is unmet. The sentence that the decisions did not depend on it is withdrawn | ADR 0014 "What this record does not do"; P5 |
| R-2: no case for cessation established after a non-kill ending | RC8 covers every ending without established cessation: once it is established the next run resets or removes, and never reports `reused`; until then every later run is refused. RC1, RC3 and RC5 are unchanged | ADR 0014 D35, D36; section 4; RC2, RC6, RC7, RC8 |
| R-3: only one bound was named | The four bounds are listed; the owner of F and the release states their relationship and values after a workable P1 route; review precedes C's budgets. No number | ADR 0014 D38; RS5; P4; C6; open item 15 |
| Q1, the qualification of a run with unresolved cessation | Decided floor: not qualifying in any tier, `qualifying: no` with the reason. The overall verdict and exit status stay F's interface obligation | ADR 0014 D34, A32, the decision 17 row; sections 6, 7 and 11; P7; C8; the spec's note for R14 |
| R-4: the kept target must be visible | `target: kept (<reason>)` in the summary of every tier; a later refusal names the earlier run from validated diagnostics or says `unavailable`. No new telemetry or alert is commissioned | ADR 0014 D34; section 11; RC2, RC6, RC7; C8 |
| Q2, the status of the two records | ADR 0013 is accepted at revision 2 under the lead's binding rulings; its status line is corrected and its body is unchanged. ADR 0014 stays proposed; the index, both headers and the spec describe a proposed partial supersession | ADR 0013 status line; ADR 0014 header and table preamble; the decision index; the spec header and note; status row A; open item 16 |
| The build-order test still prescribed one cargo invocation | The obligation is evidence from the order of commands the gate actually sets, on an empty target and after the marker changes, on macOS and native Linux, plus the isolated `concurrent_index` run. No probe is launched | the build-order case of this section (above) |

Points of the fifth amendment that the reviewers and the lead accepted as written: the end time of the warm record is taken when cessation is established; no combination of the insufficient signals of D33 establishes cessation, which does not exclude a distinct observation that is independently reviewed; the cold target has no durable trace (now P5 above); D40 holds for every tier that runs a path-coupled test; a refusal at the start of a run is `REFUSED` with exit 1, distinct from lease contention (exit 75) and from the result of a run that ended.

*Why a further review round runs.* The design had its challenge and its rebuttal, and the fifth amendment had its delta reviews. The lead decided on one more bounded round, and its reason is recorded here as the process rules ask. The delta reviews found a blocking defect that names what it breaks: the exception for content "no gate use is known to have produced" contradicts D34's rule that unknown ownership is a live writer, and it lets `full` delete a cold target under a possibly live writer, which fails R3 (TV13 against RS2). They also found the completeness and status defects in the table above. The round is limited to the same two reviewers re-checking the conditioned lines of this amendment at its exact commit, and any regression it introduced. It is not a new design round, it re-reviews no unchanged text, and it does not accept ADR 0014 by finishing: the lead records the acceptance or the rejection.

*State of the two records.* ADR 0013 is accepted at revision 2 and is the design baseline; where the lead's later rulings (K-1 to K-5, the broader coupling, and the rulings in the table above) differ from its decisions 30 and 31, the rulings bind the builders now. ADR 0014 is proposed. Where this plan calls a clause of D30 or D31 "superseded", that means overridden for the builders by those rulings today, and superseded in the decision record only when ADR 0014 is accepted. At that point only the clauses named in ADR 0014's table are marked superseded or qualified, and the rest of ADR 0013 stands.

*What stays as it was.* Every hold of this section: FS1 and FS2, the earlier security findings on the measurement supervisor, the signal and lifecycle hold, D blocked and F held, TV68, the native and integration runs, and every acceptance and timing requirement. P1 has no candidate on a macOS host where all agents share one account. P2, P3 and P6 are unimplemented, P4 has no value and P5 has no design. C, G and H stay frozen at the commits named above; C1–C9, G1, H1 and H2 are listed, not made and not authorized. The eleven filesystem controls keep exactly the credit recorded above and no more. R3, R12 and every recovery and selection vector stay in force as written; the cases this amendment marks unmet are unmet, not waived.

**Rules and merges.** `check` and `test` both stay required until the owner applies, and the lead verifies, the reviewed main-only change after PR 3 and the evidence are ready (section 7). The owner merges every pull request. This section changes no rule, no AGENTS.md text and no policy.

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
| TV13 | Control: a binary built from a passing source, the source then made failing with an older modification time, run by plain cargo on that target: passes. Then `full` on the same tree | `full` FAIL on the failing test; the planted file is gone. Sixth amendment: the expected result stands. The deletion it needs at the start of the run requires affirmative never-used provenance for the planted target (D33, RC12). None can be established until P5 exists, so this vector is unmet until then; it is not passing and not waived, and a `full` run that cannot establish the provenance is refused (RC11) |
| TV14 | The same plant in `target/gate-warm`, then `pr` | Result says `reset: content changed without a newer modification time`; `test` FAIL |
| TV15 | A warm target whose record names another worktree; a `target/gate-warm` that is a symbolic link; a warm target with the marker of a build that did not finish | Reset; refused; for the third case, by D34 and D35: where the cessation of the unfinished build's writers is independently established, `reset: previous build did not finish` (RC8); where it is not, the run is refused and the target and the marker are unchanged (RC7), also after a bounded wait ran out, with the recorded process id gone, or on an operator's acknowledgement (RC9). A marker alone never resets |
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
| TV38 | source of `orch-cli`; source of `orch-infra` | `orch-cli` and `orch-infra`, the second with a reason of class `coupled`; `orch-infra` and its dependents (which include `orch-cli`), with no `coupled` reason. Third case (D39), required in PR 2: source of a crate that `orch-cli` depends on and `orch-infra` does not selects that crate, its dependents including `orch-cli`, and `orch-infra` with a reason of class `coupled`; a member that depends only on `orch-infra` is not selected. The frozen G and H do not implement or test the third case (section 12, G1 and H1) |

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
| TV61 | Shared while shared is held | Both proceed. Required, executed in PR 2 with `fast` as the real shared requester (section 12); handshake-based, not timing-based |
| TV62 | Exclusive waiting on a shared holder; a new shared request arrives | The new request queues behind the exclusive waiter. Required, executed in PR 2 with `fast` (section 12); the acquired holder, the queued exclusive waiter and the new shared requester are each observed through a handshake, and the newcomer runs only after the exclusive waiter completes |
| TV63 | The holder is killed with SIGKILL | The lease is free at once; the stale record is replaced; no signal is sent. The holder has live descendants when it dies, and the lock descriptor is not inherited by them (RS6). The holder is the actual process that holds the lease; it is not replaced by a helper, and a measurement guardian does not stand in for it, and no helper keeps the lease descriptor after the actual holder is killed (D37; section 12, K-5). The target is not touched as a result: the next run on it is refused until cessation is established (RC7) |
| TV64 | The holder gets SIGINT or SIGTERM | Lease released; result `INTERRUPTED`. Teardown is bounded (INV-T, RS5; the bound has no number yet, D38). Where the cessation of the target's writers is independently established: the cold target is removed, the warm record is published and the next `pr` run reports `reused` (RC5). Where it is not: the cold target is left, no record is written, the result says so (`target: kept (<reason>)`, `qualifying: no`) and the next run is refused (RC6); if the cessation is established later, the next run resets or removes and never reports `reused` (RC8). A handled interrupt alone authorizes none of these (D32, D35, D36; section 12, K-1). HUP is covered as INT and TERM are (RS5) |
| TV65 | The lock file is replaced while held | The holder records the lease as lost; the run does not qualify |
| TV66 | The lock path is a symbolic link; the file has another owner | Refused with the reason; not followed |
| TV67 | A gate under `lease exec`; the same without the token | `lease: inherited`; waits |
| TV68 | A foreign process named `cargo` during the run; load data unavailable | `foreign-build-activity: yes`; `unavailable`, and the sample is not a valid measurement. A `PATH` stub is not the contract (section 12): F's own tests of its parser and sampler and a native observation cover the case, and a test that needs the stub to be taken is unmet |

Tiers (`gate_tiers`):

| # | Case | Must |
|---|---|---|
| TV70 | `--help` | Names the three tiers, `pr` as the merge condition, `full` as the goal and release gate and as the default |
| TV71 | `--plan` with no tier; with each tier | `full`; each tier's steps, target and lease mode as in section 1 |
| TV72 | An unknown tier or option; `fast` before PR 2; `--warm` with `pr` | Exit 2 with the reason. The `fast` case is stage-specific: it holds only while `fast` is unavailable, and PR 2 replaces it with `fast`'s real behaviour; it is never a permanent assertion that `fast` is unavailable |
| TV73 | A miniature run | Each row has verdict, name, wall time and detail; a total; the result file has `tier`, `part`, `run-dir` and the times |
| TV74 | `--part suite`; `full --warm` | The result says `part: suite` and never `PASSED (tier`; the legacy run says “not a gate” and cannot qualify |
| TV75 | A preflight refusal | Exit 1; the result file says `REFUSED`; the table is printed |
| TV76 | `cargo-deny` not on `PATH`, `--part static` | The `advisories` row is `ENV-LIMITED` with the install command; the run is not green |

## Requirements map

| Req | Decisions | Vectors | Tasks | PR |
|---|---|---|---|---|
| R1 | D1, D2, D4–D6, D18 | TV70–TV76 | F0, C, F | 1 |
| R2 | D7–D12, D14, D26 | TV1–TV12, TV17 | C, F | 1 |
| R3 | D8, D10–D15, D26–D30, D32–D36 | TV3, TV4, TV13–TV19; RC1–RC13 | C, F | 1 |
| R4 | D7, D9 | the comparison of the two inventories | F | 1 |
| R5 | D19, D20, D31, D39, D40 | TV20–TV38 | G, H, I | 2 |
| R6 | D19, D23 | TV21, TV25, TV32, TV34, TV50 | G, H, I, J | 2, 3 |
| R7 | D1, D6, D23, D25 | TV51–TV53, TV55 | F, J | 1, 3 |
| R8 | D24 | measured records | K, L | 4 |
| R9 | — | per-binary measurements and the changed-test ledger | D, M | 5 |
| R10 | D2, D16 | fact check of the final documents | N | 6 |
| R11 | D9, D18 | inventories and timing records | D, K, O | report |
| R12 | D1, D15 | measured wall times | D, O | report |
| R13 | — | each PR's diff and status | all | all |
| R14 | D17, D27, D34 | TV40–TV47, one real run | C, F, J | 1, 3 |
| R15 | D23 | TV54 | J, owner, lead | after 3 |
| R16 | D21, D22, D32, D36–D38 | TV60–TV68; RC5–RC7 | C, F | 1 |

## Tasks

Owners are roles. Each task runs in its own worktree and branch, never shares a `CARGO_TARGET_DIR`, and commits only the paths in its scope. Readers review a frozen commit and write only their own report.

| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| A | Design: spec, plan, ADR (architect) | R1–R16 | `docs/specs/2026-11-14-tiered-checks.md`, `docs/plans/2026-11-14-tiered-checks.md`, `docs/decisions/0013-tiered-verification-gates.md`; for the reconciliation also `docs/decisions/0014-gate-target-recovery-and-selection-coupling.md` and the index rows in `docs/decisions/README.md` | approval | links resolve; `git diff --check` | 1 |
| B | Recover the original CI failure of the daemon-harness test; hypotheses; bounded reproduction (triage) | R7, R9 | own report | approval | the original log is cited | evidence |
| D | Baseline measurements and attribution of every binary above 30 s (performance engineer) | R9, R11, R12 | own report and timing records | approval; exclusive lease while measuring | records regenerate from the logs | evidence |
| E | Challenge: can one folded run replace three without a false green (red team) | R2, R3 | own report | A | verdict on the frozen design | evidence |
| F0 | Registry seam: `Gate` entry, argument types, stubs (release engineer) | R1 | `xtask/src/main.rs`, `xtask/src/gate.rs`, stub `xtask/src/gate_selection.rs` | A, E disposition | `cargo test -p xtask`; `cargo xtask crate-graph` unchanged | 1 |
| C | Gate tests, written to fail first (test engineer) | R1–R3, R14, R16 | `xtask/tests/gate_tiers.rs`, `gate_protections.rs`, `gate_bootstrap.rs`, `gate_inventory.rs`, `gate_evidence.rs`, `gate_lease.rs`, `gate_witness.rs` | F0 | Against F0: record the expected red result, which is exit 2 “not available yet” and says nothing about any planted protection. Against implemented F: each negative case fails for its own stated reason, after its matching positive control returns 0 (section 12) | 1 |
| F | Folded gate, timings, lease, evidence (release engineer) | R1–R4, R7, R14, R16 | `xtask/gate.sh`, `xtask/src/gate.rs`, `xtask/src/gate/**` | C, D baseline, F-SIGNAL-SEAM (section 12) | C's tests pass; the old and the folded run compared at one commit (R4); `./xtask/gate.sh full` on Mac and native Linux | 1 |
| G | Selector (senior engineer) | R5, R6 | `xtask/src/gate_selection.rs`, `xtask/src/gate_selection/**` | F0 | inline unit tests; H's table. Frozen so far with inline tests and producer probes only; H against G is due (section 12) | 2 |
| H | Selection table tests, from the vectors and independent of G (second test engineer) | R5, R6 | `xtask/tests/gate_selection.rs` | F0 | fails against the stub: H rejects the stub's unavailable reason, so even the select-everything rows are red (33 of 34 outer tests; only the half-pair usage case passes, section 12); it passes only with G and shows nothing before that | 2 |
| I | Integrate the selector with `fast` (release engineer) | R5, R6, R12 | `xtask/src/gate.rs`, `xtask/src/gate/**` | F, G, H | `cargo test -p xtask --test gate_selection`; `./xtask/gate.sh fast --base <base>` on the declared workloads | 2 |
| J | CI workflow, with the `ambient` job; proposed rule configuration and rollback, for review (release engineer) | R6, R7, R14, R15 | `.github/workflows/ci.yml`, `xtask/src/linux.rs` (the test that mirrors it) | I | TV50–TV53 on real runs; TV55 | 3 |
| K | nextest pilot against the sequential baseline; 20-run qualification per platform; a series stops at its first failure, which is kept and diagnosed, and a fixed candidate starts a new series (performance engineer) | R8 | own report and records | F; native Linux; exclusive lease | records per platform | evidence |
| L | Adopt nextest (release engineer), only if K qualifies | R8 | `.config/nextest.toml`, `xtask/src/gate/suite.rs`, `xtask/src/gate/tier.rs`, `.github/workflows/ci.yml` | K, J | same inventory by name; the 20-run logs; dependency review and pinned version | 4 |
| M | Slow tests, one slice per file group (senior engineers, separate tasks) | R9 | the exact test files and trivial clock seams named after D and B | D, B | a failing regression first; assertions and timeouts unchanged; independent check by a test engineer | 5 |
| N | Rules and documentation (technical writer) | R10 | `README.md`, `AGENTS.md` (after the owner's word), the spec and plan status, the comments in `xtask/gate.sh` | implemented interfaces; all decisions | every command runs as written; fact check | 6 |
| O | Final validation, no source edits (release engineer) | R11, R12 | — | all accepted slices | `./xtask/gate.sh full` on Mac; native Linux CI; timings and inventories checked against the logs | final |

Order: A → E → F0 → C, G, H in parallel → F → I → J → (L) → N → O. B and D are independent inputs; M follows their findings. F0 is the one change to the approved order; without it the fallback of section 8 applies. “In parallel” is authoring only: the three are validated one after another on the host, and F waits for more than C (section 12).

One heavy build or measurement per host at a time. Measurement windows hold the exclusive lease; documentation, review and reading continue.

## Status

| Item | State |
|---|---|
| A | design v2 committed: challenged once, reconciled; frozen at `e96803e50c2815100e96ee55b439f392b0b5d2b4`. ADR 0013 accepted at revision 2. Reconciliation of D30 and D31 written as ADR 0014, proposed (fifth amendment). Its security and resilience reviews each returned a pass with conditions; the corrections are made as its revision 2 (sixth amendment). The same reviewers' re-check of the corrected commit and the lead's acceptance are pending; ADR 0014 is not accepted |
| F0 | committed locally as `12c263bc8e43df6bdf05efa770876ccd6eabc199`, not in a pull request. Targeted verification by its producer: `cargo test -p xtask`, `crate-graph`, `fmt --check` and 18 dispatch checks. Not run: clippy, any gate, CI, the full suite. Every tier still answers exit 2 (section 12) |
| C | frozen at `373ea896c3d6149fa5c9c43fc12f22228d57033c` (section 12; parent `412523136b3e5ab4b8efdb7146747637f28ae5fe`): N1, SR-10 and G-1 accepted by a static reading. Since the fourth amendment the eleven filesystem controls (eight target, three retention), which start no child process, have been executed on native macOS: each passed, failed under its own mutation and passed restored (section 12, fifth amendment); that is evidence for the N1 and G-1 helpers only. SR-10 has no executed control. Fixture controls on macOS only at the parent; lifecycle scenarios held; TV68 correction due; FS2 a block (independent descendant cessation, safe scratch lifetime after a normal finish); SR-1, SR-3 and SR-6 met statically with no runtime credit; recovery holds K-1 to K-4 (including cold-target removal) and holder K-5 carried into ADR 0014 D32–D37 (proposed), with the edits C1–C7 due and not made, and C8 and C9 added to that list by the sixth amendment; the numeric RS5 grace open; RC1–RC10 unrun; not accepted |
| G | frozen at `fddb1281e1b51ccfca721c84d6fe1edcd6ffff0f`: inline tests and producer probes only; implements the superseded narrower trigger of D31; the trigger of D39 (edit G1) is PR 2 work; not accepted |
| H | frozen at `6c518360cb09dbc29751c3a3c269a2c148d183c7`: F0 expected red, 1 pass and 33 fail; not run against G; edits H1 and H2 due in PR 2; not accepted |
| TV61, TV62, shared-fast RS1 | required in PR 2 (section 12); not yet executable |
| RS1–RS3, RS5–RS7, INV-T and the security and release cases of section 12 | required in PR 1; none implemented or run |
| D | held; the first measurement supervisor was blocked by each independent reviewer and its defects are open (section 12). No baseline exists |
| F, F-SIGNAL-SEAM | held (section 12) |
| all others | pending |

## Test plan

The spec's acceptance checks; the vectors above; the CI set on every pull request. `./xtask/gate.sh full` on macOS and on native Linux for PR 1 and at the end, because the work changes process handling and the gate itself. PR 1 also runs the gate of the base commit once at the same tree, for the comparison of R4. Both runs of that comparison hold the exclusive lease on an otherwise idle host. One run on each side cannot tell a failure caused by the fold from an intermittent one, so every test whose result differs is then run by itself in both environments. The report lists every difference with what those runs showed, including the ones that did not repeat; an intermittent one is triaged under R7. None of this is a rerun to obtain a pass. Timing and qualification runs hold the exclusive lease on an otherwise idle host. Reviewers use bounded disposable probes; no probe uses a real home or a real agent CLI. No user interface changes, so no tmux end-to-end test is added.

Reviews: each pull request gets independent code, security and resilience reviews of what it touches, at a frozen commit. Security covers subprocesses, paths, the untrusted diff and CI; resilience covers failure, interruption, cleanup, the lease, kept diagnostics and rollback. A fact check covers timings, inventories and public claims. One red-team pass challenges the fold (task E). A blocking finding is fixed and reviewed again or goes to the owner; it is not overruled.

Price, from the approved plan: at least 40 nextest qualification runs (20 per platform), plus baselines, pilots, the fold comparison, the negative cases, CI validation and the final gates. At an illustrative 10 minutes each, qualification alone is 400 machine-minutes; that is arithmetic, not a measurement.

## Rollback / compatibility

No product data, on-disk format or protocol changes. Each pull request is reverted by itself, in reverse order of dependency; reverting PR 1 restores the three-run script. The rules change has its own rollback: the saved ruleset. `target/gate-result.txt` keeps its place and its three states and gains lines; a reader that looks for `verdict:` still works. `--warm` and `--keep` keep working. The new directories (`target/gate-warm`, `target/gate-runs`) are inside `target/` and ignored by git. No test is deleted and no failure is hidden to meet a target.

## Docs to update

- PR 1: the header comment of `xtask/gate.sh`; `docs/decisions/README.md` (index rows for 0013 and 0014; both were added with the fifth amendment and are updated when a status changes). One sentence of AGENTS.md becomes untrue in PR 1 (“its `advisories` step silently passes when cargo-deny is missing”); see the open items.
- PR 3: the comment at the top of `.github/workflows/ci.yml`.
- PR 6: AGENTS.md (pre-PR check, workflow, release step, roles), README “Development”, this plan and the spec set to `done`. The text on what each tier proves must carry the stated limits: no tier runs the ambient configuration and the `ambient` job reports after the merge (D25); a gate refuses opt-in test variables (D26); `real-home` may be `partial` (D28); `no-real-cli` and `home-hygiene` do not see a child with its own environment (D12, D29). `docs/RUNBOOK.md` has no developer check section today and needs none.

## Open items

For the lead; none blocks the design challenge.

1. F0 (section 8): settled. The seam-first commit is adopted and committed locally on the PR 1 branch (section 12).
2. The test engineer's scope grows from two files to seven (section 8). They are disjoint from every other task.
3. AGENTS.md in PR 1: settled. The one sentence about `advisories` is corrected in PR 1, in the same change that makes it false, by the technical writer, who also owns the decision index row; the broader verification policy stays in PR 6.
4. D13 can fail because another `orch` process changed the real global directory during a gate. If that proves frequent, it needs the owner: accept manual attribution, or run gates with `orch` idle.
5. D8 and D26 keep `ORCH_*` out of the suite's environment. A test that relied on an inherited variable will show up in the R4 comparison. The session list of D26 is a constant; when `orch` starts exporting a new variable to agent sessions, a gate started from such a session is refused and names it, and the list gains a line.
6. Unverified and settled only by running: GitHub's treatment of a job skipped by condition under this ruleset (TV50); nextest's reading of `[env]` (section 3); file locking and `ps` on Linux (F13, section 6); the cost of a from-nothing build of `xtask` (D27); that creating a file changes its parent directory's modification time on both platforms (TV18 c).
7. D25: settled. The `ambient` job runs on every push to `main`, never on a pull request (A19 is not taken). It is new work in task J.
8. A suite that hangs holds the exclusive lease. Settled for now: bounded termination (INV-T) is a PR 1 obligation; an internal suite deadline is optional and can only fail the run (section 12).
9. The inventory parser reads one merged log (section 3). It fails closed, and the merged-output cases of section 12 are required tests. Whether to parse two streams is for the code review of PR 1.
10. PR 3 decides and measures what the CI cache saves (section 7).
11. Possible follow-up, not in this plan: a source check that flags a `PATH` with host install directories in test code (D29).
12. D31 broader coupling (section 12): the lead's decision is carried into ADR 0014 D39 and D40, proposed. Open: the review and acceptance of ADR 0014; the selector change G1, the test edits H1 and H2, the two-marker probe on macOS and native Linux and the isolated `concurrent_index` run, all PR 2 work and none started.
13. N1 (target path comparison), SR-10 (the interrupt fixture) and G-1 (scratch retention while unwinding) are accepted by a static reading in C `373ea896c3d6149fa5c9c43fc12f22228d57033c`. The eleven filesystem controls of N1 and G-1 and their mutations have since been executed on native macOS (section 12, fifth amendment); that is not evidence for the gate. SR-10 has no safe executable control before the lifecycle seam exists and is unrun. The TV68 observation seam is due in C and F before PR 1 (section 12). The signal and lifecycle hold has no resolution yet.
14. K-1 to K-4 (reuse after a handled interrupt, reset after a hard kill, the outcome of a bounded wait, the scope at normal completion, and the removal of the cold target after a handled interrupt) and K-5 (holder identity) are reconciled with D30 in ADR 0014 D32–D37, proposed (section 12, fifth amendment). The decisions say what the gate must not do without established cessation. They do not say how cessation is established: prerequisite P1 is open, has no candidate on a macOS host where all agents share one account, and blocks F and PR 1. The test edits C1–C7 are due and not authorized yet.
15. The numeric bounds are unmet and deliberately unchosen (D38, P4): the handled-interrupt grace, the wait for children after a signal, and the waits for cessation at the end and at the start of a run. The original owner of F and the release states whether they are distinct or one bound and supplies every supported value once P1 has a workable route, the reviewers assess the set, and C's budgets follow it (section 12). The runbook for retained `/tmp/gate-contract-*` scratch is future work.
16. ADR 0013 is accepted at revision 2, the design baseline, under the lead's binding rulings; its status line says so since the sixth amendment and its body is unchanged. ADR 0014 is proposed, at its revision 2. Its acceptance needs the same security and resilience reviewers' re-check of the corrected conditions at the exact commit, and the lead's recorded acceptance; finishing a review task does not accept it. At acceptance only the clauses named in its table are marked superseded or qualified, in ADR 0013's status line and in the index, and the rest of ADR 0013 stands.
17. Decided by the lead and carried by the sixth amendment: a run whose cessation is unresolved does not qualify in any tier (D34, P7). Still open: the overall verdict and the exit status of such a run and the fixed substrings (F's interface obligation); the durable provenance P5, which is load-bearing, has no design and blocks every start-of-run deletion of an existing cold target, TV13 included; the wording of the refusal for unresolved writers and for unknown history (section 11). Confirmed by the reviewers and the lead: the end time of the warm record is taken when cessation is established (D35), and no combination of the insufficient signals of D33 establishes cessation.
18. D40 is stated for every tier that runs a path-coupled test. Whether section 3's `--no-run` command builds the `orch` binary for a `-p` selection is unverified until the two-marker probe runs.
