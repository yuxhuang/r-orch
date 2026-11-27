# AGENTS.md

Read this first, whatever your role. Reasons live in the linked docs.

## What this is

r-orch is a Rust AI-agent orchestrator: a CLI (`orch`), a terminal dashboard and a runtime that drives external
AI coding CLIs as child processes and keeps all state as plain files under `.orchestry/`. It began as a port of
`oxgeneral/ORCH` and is developed on its own terms.
The repo is **public**.

| Crate | Role |
|---|---|
| `orch-domain` | Pure types and functions. No I/O, no async. |
| `orch-core` | Port traits: storage, adapter, process, workspace, clock, ids. |
| `orch-infra` | Implementations of the `orch-core` traits (files, adapters, processes). |
| `orch-engine` | Services, orchestrator, event bus, container. |
| `orch-peer` | Remote-peer protocol: links, mirrors, reconnect. |
| `orch-cli` | clap command tree, human and `--json` rendering, serve mode, `orch web`, `orch mcp` (stdio MCP server, `src/mcp/`). Binaries live here. |
| `orch-tui` | ratatui dashboard. |
| `xtask` | Dev tooling: crate-graph, gate, fixture-gen, linux, changelog. Run as `cargo xtask <cmd>`. |

Dependency direction is asserted by `cargo xtask crate-graph` (ARCHITECTURE §3.1).
Authoritative docs:

| Question | Read |
|---|---|
| How is it built, which crate owns what, which traits | [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) |
| How the engine behaves at runtime: goals, tasks, handoffs, pause; how adapters drive agent CLIs | [docs/internal/](docs/internal/README.md) |
| Something is broken at runtime | [docs/RUNBOOK.md](docs/RUNBOOK.md) |
| What users see and how they install | [README.md](README.md) |
| How to use every feature, step by step | [docs/manual/](docs/manual/README.md) (regenerate the command reference and screenshots with `docs/manual/tools/`) |
| What changed per release | [CHANGELOG.md](CHANGELOG.md), assembled at release from fragments in `docs/changelog/unreleased/` |
| Which releases lead to 2.0, and which platform they are built and qualified on | [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) §13 |
| Why we chose X | [docs/decisions/](docs/decisions/) |
| What a piece of work must do, and how we check it | [docs/specs/](docs/specs/) |
| How and in what order we will build it | [docs/plans/](docs/plans/) |
| Frozen trait signatures | [docs/TRAITS-FROZEN.md](docs/TRAITS-FROZEN.md) |
| Historical: the port-era gate reviews | `docs/reviews/` |

## Non-negotiables

1. **Pinned toolchain.** `rust-toolchain.toml` pins 1.99.1 (edition 2024). Do not override it or bump it casually
   (see Don'ts). Only rustup honours the pin: if `rustup which cargo` fails, your `cargo` (e.g. Homebrew) ignores
   it and your clippy/fmt results will differ from CI. Match CI (install rustup).
2. **Green before a PR:** the CI set, run locally (see Pre-PR check). A red PR is not reviewed or merged.
3. **Stable sorts only.** `clippy.toml` bans `sort_unstable`, `sort_unstable_by`, `sort_unstable_by_key`. Index
   order decides which agent is picked, so stability is behaviour. Where a sort is provably not over index
   order and speed matters, use `#[allow(clippy::disallowed_methods, reason = "...")]`.
4. **Library-first (ARCHITECTURE §4.8).** If a maintained crate covers a complex feature, use it; do not
   hand-roll. To hand-roll anyway, state in a code comment which exemption applies: no maintained crate fits, or
   a still-binding compatibility requirement rules it out. Adding a dependency: do the §4.8 check (maintenance,
   MSRV of the pinned toolchain, licence via `deny.toml`), add it to `[workspace.dependencies]`, and write an ADR if it is
   load-bearing (storage, parsing, process, networking, rendering).
5. **Never touch the developer's real `~/.orchestry`.** `.cargo/config.toml` sets
   `ORCHESTRY_GLOBAL_CONFIG_PATH` to `target/test-global/global.yml` for every cargo command, because tests that
   wrote the real global config raced each other and corrupted it. Keep the basename `global.yml`. Tests use temp
   dirs; never `set_var` the home in a test (process-global, racy). Do not unset that env var.
6. **No secrets and no private infrastructure names in the repo.** That covers tokens, personal machine paths,
   private Homebrew tap names, private hostnames and IPs. Use placeholders (`<tap>`, `<host>`).
7. **The repo stays public.** Do not change visibility, publish crates (`publish = false` is deliberate), or
   push to any other remote. Open-sourcing is the owner's decision.
8. **Parity validation is gone.** There is no oracle, parity contract or differential harness; behaviour is
   specified by r-orch's own docs and tests. See [ADR 0009](docs/decisions/0009-remove-parity-tooling.md).
9. **Advisories go to the owner.** A new RUSTSEC advisory that reddens CI (the weekly scheduled run exists to
   surface this) is escalated, never silenced by the agent. A `deny.toml` `ignore` entry needs owner sign-off
   and the reasoned-comment format of the two existing entries: why no replacement, what bounds the exposure.
   Until `SECURITY.md` exists, report a vulnerability you find to the owner privately (GitHub private
   vulnerability reporting or direct message), never in a public issue, PR title or PR body.
10. **Linux only until 2.0.** By the owner's ruling of 2026-11-23, every release from 1.1 through 2.0 is
    developed, tested, gated and qualified on Linux only: GitHub's Linux CI, Linux development hosts and the
    Linux cold gate. No macOS check, Darwin or APFS row, macOS peer or hardware proof, or macOS suspend test
    is required by a PR, a spec, a review or a release before 2.0, and none is run for them. The Linux
    fenced-lease sleep/wake check stays: a supervised process through a real suspend and resume of an isolated
    Linux host that the owner has authorized (spec R42, issue 110; pending, and nobody suspends a host without
    the owner's permission). The injected-clock test proves renewal ordering only. A failure or gap
    that occurs only on macOS is a GitHub issue in the
    [After 2.0](https://github.com/yuxhuang/r-orch/milestone/20) milestone (unless triage assigns another),
    not a blocker. A failure that Linux shows too is an ordinary Linux failure. This changes no code or CI and
    removes no macOS code path; the second host for the peer check is a Linux host, and Linux CI, the review
    rules and owner-only merges are unchanged. macOS compatibility is worked on after 2.0. Because macOS goes
    unqualified, the README upgrade notes, the `CHANGELOG.md` entry and the release PR of every release through
    2.0 say so (see Release). See ARCHITECTURE §13
    and record K of the [SQLite storage spec](docs/specs/2026-11-20-sqlite-storage.md).

## Licensing

- r-orch is licensed under Apache-2.0 ([LICENSE](LICENSE), attribution in [NOTICE](NOTICE)); `Cargo.toml`
  declares it for every crate. Rule 7 still applies: the licence does not change the rules on visibility, crate publishing or remotes.
- Contributions are accepted under Apache-2.0 itself (§5, "Submission of Contributions"). There is no CLA and no
  DCO sign-off.
- Material that originates outside this project (copied or adapted files, vendored assets, fixtures recorded
  from another project) needs a permissive, Apache-2.0-compatible licence, and goes into
  [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) (component, upstream, licence, files, full licence text) in the
  same PR. Add to `NOTICE` only what that file's rule requires. Crate dependencies are covered by `deny.toml`
  instead (rule 4).
- Before merging work from outside the project, confirm with its submitter that they wrote it or have the right
  to contribute it, and where any copied part came from.
- A built binary, archive or package must carry `LICENSE`, `NOTICE`, `THIRD-PARTY-NOTICES.md` and the licence
  notices of the crates it links (THIRD-PARTY-NOTICES.md, "Distributing r-orch"); the root files alone do not reach
  an installed binary.
- Do not change the licence or the copyright line; those are the owner's decisions.

## Workflow

Everything reaches `main` through a PR. A GitHub ruleset enforces this, needs 0 approvals, and requires the
`check` and `test` CI jobs to pass. Both stay required until the owner applies a reviewed rules change (see "What
gates a merge"). The same ruleset protects `release/**`, so do not create branches under that prefix. A red PR is not
reviewed or merged.

1. Branch from current `main`: `fix/<slug>`, `feature/<slug>`, `docs/<slug>`, `chore/<slug>`, and
   `chore/release-vX.Y.Z` for releases.
2. Commit as `type(scope): summary`, imperative, no trailing period. Types in use: `feat`, `fix`, `docs`,
   `test`, `chore`, `refactor`. Scope is a crate or area: `fix(peer): bound a worker's free-form error text`,
   `chore(release): 0.5.1`. The summary says what changed in behaviour. Process labels ("integrate verified v6
   b0 slice") do not belong there.
3. Run `fast` after each commit, and the pre-PR check (see Verification tiers) before opening the PR.
4. Open the PR using `.github/PULL_REQUEST_TEMPLATE.md`; fill every section, delete none. One concern per PR;
   if the title needs "and", split it. Normally keep a PR under about 4,000 changed lines. Split a PR over about 5,000
   changed lines, or explain in the plan why it cannot be split.
5. CI (`.github/workflows/ci.yml`, Linux only) must be green at the PR's final head, once (see "What gates a merge"):
   `changes` classifies the PR, `check` runs fmt, clippy `-D warnings`, `cargo xtask crate-graph` and cargo-deny (plus
   a workspace build for a documentation-only PR), and `test` runs the folded suite once for a code PR. A new push needs
   a new run.
6. Merge, per the policy below. Implementers and reviewers never merge their own work.
   - Convention: squash for single-concern PRs (the squash title becomes the commit; make it a good one);
     merge commit for release PRs and multi-commit PRs whose commits are individually meaningful.
   - Never force-push `main`.

### Owner’s review and merge rules of 2026-11-27

- Each PR gets one independent code review. Add a security review only for untrusted input, file paths,
  authorisation, importer, restore or process spawning. Skip separate per-PR resilience reviews, fact-checks,
  review boards and CI-log verifier tasks.
- Require green Linux `check` and `test` on the actual merge (`test` may skip for a CI-classified docs-only PR),
  a `CLEAN` merge state and a short lead recommendation naming the head, base and merge tree.
- If `main` moves and the unchanged head still merges cleanly, rerun only CI on the new merge. Review the delta
  when the head changes. Use batch trains for independent PRs. Once #381 lands, CI fails fast on stale merge parents.
- The supervising session checks each squash and watches the push to `main`. Skip separate landing and
  terminal-main verifier tasks.
- **Rolling deep review.** Review each subsystem’s changes on `main` since its last deep review after about
  10 merged PRs or 10,000 changed lines. A rolling review never blocks a merge. Its only output is GitHub
  issues. File each finding in `yuxhuang/r-orch` and follow `issue-triage-process.md`. Triage assigns priority
  and milestone, and the supervising session accepts or escalates the determination. The lead picks up findings
  through triaged issues and their `[issue N]` fix tasks. The reviewer sends no findings, recommendations or
  fix requests directly to the lead. Close or explicitly defer each finding by the next milestone.
- **Milestone deep review.** Plan a deep review as an acceptance step for each of T10 acceptance, T24
  activation and T27 release. Report the verdict and findings directly to the lead, who uses them to decide
  milestone acceptance. File a GitHub issue for each finding that will remain open at the gate.
  Do not schedule a deep review per train or batch.

### Independent PRs, stacks and trains

- **Base on `main`.** Branch every PR from current `main`. Independent PRs stay independent and are qualified
  together in a *train*: an ordered batch checked as one combined merge on a draft branch from green `main`, which
  the owner then merges member by member, in that order. The draft is closed unmerged once `main` has the
  recorded tree.
- **Stack only for code.** Base a PR on another open PR only when the upper change needs the lower PR's code. Say
  so in both PR bodies (`Depends on #N`). The whole stack enters one train, bottom to top (#171 then #175, #186
  then #208). If any member is held, the whole stack waits.
- **A changed head restarts the gate.** A new head (a push or a rebase) needs a delta review, fresh CI at that
  head and a renewed lead recommendation. Evidence for the old head does not carry over.
- **One writer for shared documents.** Within a wave of parallel work, one documentation writer owns the shared
  specs, plans, ADRs and this file; builders own disjoint implementation paths, named in each task's scope. A
  builder who needs a shared document changed asks the writer instead of editing it.
- **Release notes never conflict.** A PR adds a changelog fragment, not a line in `CHANGELOG.md` (see
  "Changelog fragments").

**Merge policy.** The human owner merges by default. The owner may standing-delegate categories to the
orchestrator; current delegation: docs-only, test-only, and CHANGELOG/plan-status PRs, once required checks are
green. The exception is a change to the process itself (`AGENTS.md`, `docs/decisions/`, and the `docs/specs/` or
`docs/plans/` README and TEMPLATE files), which the owner merges. Everything else waits for the owner. Changes to
the delegation are recorded here, in this paragraph.

### Verification tiers

`./xtask/gate.sh [tier]` has three tiers. Each gives a different amount of evidence for a different purpose, and
none replaces the one above it. The commands below are the implemented interface; the rules for who runs what are
in the sections after the table, each marked with its status (see "Status of each rule").

| Tier | Command | What it runs | Target and lease | Use |
|---|---|---|---|---|
| `fast` | `./xtask/gate.sh fast --base origin/main` | `fmt`, workspace `clippy -D warnings`, `crate-graph`; then, for a code change, `build`, `test`, `home-hygiene`, `no-real-cli` and `real-home` on the selected packages (plus `environment-witness` when the selection is a package list without `xtask`); for a documentation-only change, a workspace `build` and no tests | `target/gate-warm`; shared lease | Local iteration, every commit and fix round. Never merge or release evidence: its result always says `qualifying: no: local iteration; never merge evidence`. |
| `pr` | `./xtask/gate.sh pr` | All nine steps in one run: `build`, `test`, `clippy`, `fmt`, `advisories`, `crate-graph`, `home-hygiene`, `no-real-cli`, `real-home`. The `test` step is one whole-workspace suite run under a sentinel `HOME` and recording agent shims, judged for failures, zero tests and nonzero named inventory. It is two `cargo nextest run` passes (everything, then the one binary that needs the host to itself) plus one `cargo test --doc`, judged together as one inventory over the union | `target/gate-warm`, reused only after a content check; exclusive lease | The merge-condition tier. CI runs its suite steps as `pr --part suite`; its separate `check` job runs the equivalent static commands (`cargo fmt`, `clippy`, `cargo xtask crate-graph`, cargo-deny) directly, not `pr --part static`. |
| `full` (the default) | `./xtask/gate.sh` | The same nine steps as `pr` | Cold `target/gate`, removed afterwards unless `--keep`; needs 12 GiB free; exclusive lease | The goal and release gate, once per goal or release, not per PR. |

`--plan` runs no step: it prints the tier, target, lease and steps. Through `./xtask/gate.sh` it is not free of side
effects, because the wrapper acts before any argument reaches the judge: it takes the host lease (or inherits one from
`gate lease exec`) as `fast` or `pr`/`full` would, so `pr --plan` waits up to `--lease-wait` minutes behind a running
gate and exits 75 if the lease stays busy; it takes the use lock `target/.gate-xtask.lock` (exit 1 if busy); and it
builds the `xtask` judge into `target/gate-xtask` when that is missing or stale, then stamps it.

A run that reaches the judge prints a summary table with each step's verdict, wall time in seconds and exit status. It
replaces `target/gate-result.txt` with `verdict: IN PROGRESS` as soon as it starts and with the same text as the table
when it ends, so a killed run leaves `IN PROGRESS`. A wrapper refusal (exit 75 lease busy, or a bootstrap failure with
exit 1), a usage error (exit 2) and `--plan` write no result: `target/gate-result.txt` may then be an older run's, and
may even say `qualifying: yes` for another commit. Check its `commit` line before you trust it.

Exit status: 0 all steps passed, 1 a step failed or the run was refused, 2 bad arguments, 3 nothing failed but a step
was `ENV-LIMITED` (for example `cargo-deny` is missing, so `advisories` was not evaluated), or `--evidence` found the run
non-qualifying, or `--receipt` found the receipt unsuccessful, 75 the cooperative lease was busy and nothing ran, 128
plus the signal after an interrupt. Exit 0 alone is not qualifying evidence: look for `qualifying: yes` in the result.
It is printed only for a complete, clean-tree run of the whole tier that the wrapper started, at an unchanged head, with
the default lease directory, the pinned toolchain, a held lease and a finished target use. `fast` is never qualifying, and
`--part`, `--warm` and `--keep` each disqualify a run (`--keep` keeps the cold target of `full`).
Any other disqualifier is named after `qualifying: no:` in the result.
Options: `--base <rev>` (fast; default `origin/main`), `--part suite|static` (pr and full only), `--evidence <dir>`,
`--receipt <dir>` (`pr --part suite` only), `--lease-wait <minutes>` (default 30), `--host-label`, `--keep`, `--plan`,
and `--warm` (full only: reuses your `target/`, developer-only, never a gate). `--no-lease` is refused: a gate must hold
the lease.

Time **targets** (spec R12), not measurements: `fast` at most 5 minutes for a single-crate change and under 1 minute
for documentation only; `pr` at most 20 minutes; `full` at most 25 minutes. None has been measured at this commit.

**Parallel test binaries: `cargo-nextest` is the default runner.** On 2026-11-24 the owner reversed the deferral of
2026-11-17 and adopted the tool, and amended the bar to **5** consecutive green runs on this Linux host (decision 24 of
[ADR 0013](docs/decisions/0013-tiered-verification-gates.md) asked for 20 per platform). The decision and its design are
[ADR 0022](docs/decisions/0022-adopt-cargo-nextest.md).

- **Install it:** `cargo install cargo-nextest --locked --version 0.9.152`. The version is pinned. A missing or
  different version **fails the gate closed** with that line and the rollback; there is no silent fallback.
- **There is one runner.** The suite step has no runner option: asking for one is a usage error. The rollback is to
  revert the change that adopted nextest, which is why it lands as one reviewable branch. Two commands still use
  libtest on purpose and are not a fallback for the suite: `fast`'s `environment-witness` step
  (`cargo test -p xtask --test gate_witness`, one independently inventoried test) and `cargo xtask linux` inside its
  container.
- **Isolation lives in `.config/nextest.toml` only**, in three forms, cheapest first. nextest runs every test in its
  own process, so an in-process mutex no longer serialises anything. A test whose in-process lock guarded a
  per-binary resource gets a `max-threads = 1` test group; a binary whose tests race *each other* for a host-wide
  resource but not the workspace gets **a pass of its own** (below); a test that observes a host-wide resource gets
  `threads-required = "num-test-threads"`, at test level where only one test needs it. Never split, rename, reorder
  or skip a test under `crates/**` to suit the
  runner, and never add a retry: a flaky test is fixed or named. `xtask/tests/nextest_isolation.rs` enforces this: it scans
  `crates/**` for process-global locks and host process-table reads and fails unless each one is either named in
  `.config/nextest.toml` or carried in its ledger with the reason it needs no entry. A new test that needs isolation
  needs a line in that file, with the file and line of its evidence, like the entries already there.
- **The suite runs in two passes**, because orch-cli's `remote_peers` releases an ephemeral port and rebinds it
  across a restart: it needs the host to itself, but its own 42 tests must still run in parallel, which is the
  condition it was green under when cargo ran one test binary at a time. So one `cargo nextest run` takes everything
  except that binary and a second takes only it. The split is defined **once**, as the two profiles' complementary
  `default-filter`s in `.config/nextest.toml`; the gate, the `ambient` CI job and the guard all read that one
  definition, and the guard fails if any of them disagrees. It cannot hide a test: the expected set comes from one
  **unfiltered** `cargo nextest list --ignore-default-filter`, and the gate judges the **union** of both passes
  against it, so a test no pass ran has no result and a test two passes ran is a duplicate. Both refuse. Each
  pass's exit status is judged against its own stream, so one pass's failure cannot be answered by the other's
  exit. The second profile may be nothing but `inherits = "default"` and its half of the split: with two
  profiles, a setting in the wrong one escapes a whole-file check, so the guard names every table and key the
  file may have.
- **The speedup that counts is on the development host**, and the only timing taken is from before this
  configuration existed: `cargo test --workspace --no-fail-fast` at 1014.7 s against a two-pass nextest prototype at
  about 400 s (roughly 2.5x), the prototype having made *more* binaries exclusive than the shipped configuration
  does. So that figure is an upper bound on the exclusive work, **no timing is claimed for the configuration as it
  ships**, and no improvement on a GitHub runner is claimed either (CI also pays to build the tool). Composing those
  samples **estimates** about 335 s for the two passes plus the doc-tests (~175 s of parallel work, ~135 s of
  exclusive tests inside the first pass, ~20 s for the alone pass, 4.2 s of doc-tests), roughly 3x; it is arithmetic
  over two different samples, not a run, and [ADR 0022](docs/decisions/0022-adopt-cargo-nextest.md) carries the
  per-component sources. The owner's bar is 5 consecutive green runs on this Linux host, which is a flakiness bar,
  not a timing one.
- **Not resolved by the adoption:** the tool's dependency audit, whose 23 advisories and 10 licence items sit in
  nextest's own tree rather than `Cargo.lock`, so `cargo deny` never saw them and the `advisories` step is
  unaffected. The owner accepts nextest as an out-of-tree development tool; no licence allowance or advisory
  `ignore` is authorized, and that security review is separate.
- The time targets above are still targets: a green tier is not a speed result.

#### Status of each rule

- **Implemented** means the command or check exists in `xtask/` and `.github/workflows/ci.yml` as described.
- **Approved policy** means the owner approved it (the goal's questions Q1 to Q5, the unavailable-host answer and the
  final plan approval) but no code or GitHub setting enforces it; people and agents follow it. Approved policy is
  stated as approved: it is not held back for new owner wording.
- **Awaiting the owner** means a GitHub ruleset change only the owner applies. Until the owner has applied it and it
  has been read back, the existing rules stand: both `check` and `test` are required on `main` and `release/**`.

#### What each tier selects

`fast` compares your tree with the merge base of `--base` (staged, unstaged and untracked paths included) and prints
each changed path with its class, what it selects and why. **Implemented.**

- A path inside a workspace crate selects that crate and every crate that depends on it. A selection that contains
  `orch-cli` also selects `orch-infra`, because `concurrent_index` starts the selected `orch-cli` binary by path.
- Documentation (`docs/`, top-level `*.md`, `LICENSE`, `NOTICE`) selects no tests. `docs/ARCHITECTURE.md` is the
  exception: it selects `xtask`, whose tests read it.
- Any `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` or `rust-toolchain`, `clippy.toml`, `deny.toml`, `.gitignore`, `.gitattributes`,
  and everything under `.cargo/`, `xtask/`, `.github/`, `.config/`, `fixtures/`, `skills/`, `examples/` or shared test
  support selects everything.
- It fails closed: an unknown path, an empty change set, an unreadable base or any parse error selects everything.
- It has a known gap: the `orch-engine` guard `token_eco_preparation::dispatch_is_the_only_production_caller_of_adapter_execute_and_awaits_preparation_first`
  reads every crate's source without a Cargo dependency edge, so a change in a crate that `fast` selects without
  `orch-engine` can break it unseen. The `pr` and `full` tiers run the whole workspace.

```
./xtask/gate.sh fast --base origin/main          # not run for this document
cargo xtask gate select --base origin/main --format json   # selection only: no test runs and no workspace crate is built
```

`cargo xtask` is the alias `cargo run --package xtask --`, so cargo compiles `xtask` and its dependencies when they are
out of date, into your cargo target directory (by default `target/`). The selector itself only runs `git` and `cargo metadata`.

#### Builders (status: `fast` implemented; per-round use approved policy)

1. Run `fast` after every commit and every fix round. Read the selection it prints; `all` is the correct answer for a
   shared input.
2. Before opening a PR, run the pre-PR check below. It is unchanged: `fast` does not replace it, and `pr` does not
   replace it until the owner says so.
3. Run `full` yourself only for changes to layering, process handling or security-relevant code (the standing rule),
   and for a release or the once-per-goal run. Do not run it per PR.
4. In the PR, name the tier or command, the platform, the commit, and what that tier did not run.

#### Reviewers (status: approved policy, from Q1, Q2 and the unavailable-host answer)

A reviewer reads evidence and does not merge. Two questions need different evidence.

**What a reviewer may cite.** Any evidence that names its commit, for the claim it supports and with its scope stated:
a fix round's `fast` result, a `--part` run, a diagnostic receipt, a builder's commit-stamped report, and commands the
reviewer ran itself with their output. A `fast` result shows that the selected crates passed; it does not show that the
merge condition is met.

**What counts toward the merge condition** (the evidence the owner merges on). Only:

- the PR's CI run **at its final head** (`check` and, for a code change, `test`), with the commit it ran at;
- a verified evidence bundle from a `pr` or `full` run (`cargo xtask gate evidence verify <dir> --commit <sha>`)
  that says `qualifying: yes`, where the section on native Linux below allows it.

These do not count toward it, however useful they are as review evidence: any `fast` result; exit 0 without
`qualifying: yes`; a run at an earlier head (a push after a green run needs a new green run); a `--warm` or dirty-tree
run; a `--part` run or a diagnostic receipt standing alone (a receipt shows what a suite run did, not that the whole
gate passed; CI's own `test` job is a `--part suite` run with a receipt, and counts as part of the PR's CI run, not by
itself); a CI job that was skipped without the documentation-only classification; or a builder's report with no commit.
A reviewer states what was checked and what was not, as in Roles.

#### What gates a merge (status: Q1 approved policy; live rules unchanged)

- **Approved policy.** One green Linux execution at the PR's final head is the CI evidence a merge needs. A failed run
  is kept and diagnosed, never re-run until it passes; a flaky test is fixed or named, not retried. This replaces the
  earlier rule of three complete green CI executions per PR, which the repository records for the daemon-owned-state
  goal in [its spec](docs/specs/2026-11-09-daemon-owned-state.md) and [plan](docs/plans/2026-11-09-daemon-owned-state.md); both
  already carry the owner's amendment of 2026-11-13 that one green execution is enough. Independent review remains.
- **Live rules today (Awaiting the owner).** The ruleset requires the `check` and `test` jobs on `main` and
  `release/**`; the last read-only capture (2026-11-17) found them unchanged, and a capture is a record, not a live
  query. The owner's approved change removes only `test` on `main`; it is not applied, and no agent applies it.
  A `<linux-host>` run (below) therefore does not satisfy the required `test` job on a code PR; wait for GitHub's.
- **Documentation-only PRs. Implemented.** The `changes` job classifies the PR with `gate select`; for `docs-only`
  the `test` job is skipped by its `if` condition while `check` still builds and runs fmt, clippy, `crate-graph` and
  cargo-deny. GitHub treats a conditionally skipped job as satisfying a required check
  ([docs](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/control-jobs-with-conditions)).
  A whole workflow skipped by path or branch filtering leaves its checks pending and blocks the merge
  ([docs](https://docs.github.com/en/pull-requests/collaborating-with-pull-requests/collaborating-on-repositories-with-code-quality-features/troubleshooting-required-status-checks)),
  so the workflow has no path filters. A natural documentation-only PR under the
  live rules has not yet been observed (acceptance is owed); report what you see.
- **After the merge.** A push to `main` forces `all` and also runs the `ambient` job: the ordinary
  two-pass `cargo nextest run --profile default|alone --workspace --no-fail-fast` plus
  `cargo test --workspace --doc --no-fail-fast`, from the
  repository root with the repository's `.cargo/config.toml` `[env]`. Every step after the first carries
  `if: ${{ !cancelled() }}`, so a failing pass does not hide the other pass's result or the doc-tests': the
  job exists to be diagnosed, not to stop early. `xtask/tests/ambient_env.rs` is what proves that
  `[env]` reaches the test processes a runner starts itself. No tier run before the merge runs that configuration, and
  `ambient` does not run on PRs: it reports after the merge. A weekly schedule runs `check` alone.

#### The full gate (status: implemented; once per goal and release is approved policy)

`full` runs the suite once, not three times: the sentinel-`HOME` and recording-shim checks are judged in that one run.
Run it from a clean, committed tree. It proves:

- **No stale artifact:** a cold `target/gate` is built for the run; `pr` and `fast` reuse `target/gate-warm` only after
  a content check against the source.
- **Real tests ran:** the suite's exit status is judged, and a run with no executed test fails. The inventory names the
  tests: the names come from `cargo nextest list`, and a test the listing reports as filtered for any reason but
  `ignored`, or a binary the listing could not query, refuses the run. Cargo's own `--no-run --message-format=json`
  artifact map stays the independent expected set: the two must name the same test binaries, so the runner does not
  supply both sides.
- **`home-hygiene`:** the suite ran from outside the workspace under an empty sentinel `HOME`, and nothing but
  `.cargo` and `.rustup` appeared in it.
- **`no-real-cli`:** no test reached a recording shim standing in for an agent CLI.
- **`real-home`:** a metadata-only snapshot of `~/.orchestry` is unchanged. Coverage is bounded (depth 4, 5,000
  entries); a bounded comparison prints `partial` and says which bound it reached.

What no tier proves, and what is accepted rather than fixed:

- No tier runs the ambient configuration; the `ambient` job reports it after the merge.
- A gate refuses to start when an opt-in `ORCH_*` or `ORCHESTRY_*` test variable is set (it names them): those tests are
  not part of a gate and run directly with cargo.
- `home-hygiene` and `no-real-cli` do not see a child process a test starts with its own `HOME` or `PATH`.
- A process that a test or build script left behind, and that the gate did not start, can write into a target that a
  later run trusts, and no tier detects it (the owner's accepted risk, ADR 0015 decision 41). Never remove `target/`,
  by `cargo clean` or by hand, and never delete a gate marker while a command of a killed gate may still run. The
  refusal text says not to remove `target/` while a process of an earlier use is alive, and to inspect the marker
  read-only with `lsof`. Never deleting a marker is this file's rule.
- Passing proves nothing about timing, and a green `fast`, `pr` or `full` is not a performance result.

#### Native Linux and the unavailable-host fallback (status: Q2 approved policy)

GitHub's Linux CI is the default evidence. The owner approved that a suite run on `<linux-host>`, the native Linux
worker (never write its real name in this repository), may replace GitHub's suite evidence only when its record shows a
clean tree at the exact PR head before and after, the pinned tools and configuration, a warm target unique to the
worktree, the lease, complete logs, per-step exit status and the named test and doc-test inventory, and the PR links
that record. Without that record,
GitHub's run is the evidence. When `<linux-host>` is unavailable, rely on GitHub's CI for an ordinary PR and
say which local check did not run. Through 2.0 no macOS check is run or reported as not run (rule 10). This does
not waive the cold `full` run on Linux for a goal or release, and it does not change a live rule (above).

#### The cooperative lease (status: Q5 approved; implemented)

Gating runs take one host-wide cooperative lease (a lock file under `/tmp`; the default directory is part of what makes
a run qualifying). `fast` takes it shared, so fast runs in different worktrees coexist; `pr` and `full` take it
exclusively. `--lease-wait` is in minutes; a busy lease exits 75 and runs nothing. The refusal prints unvalidated
diagnostics about the holder. `cargo xtask gate lease status` observes it, and `cargo xtask gate lease exec --purpose
<text> -- <command>` holds it exclusively for another command. The lease is cooperative:
it does not stop a process that never asks for it (a bare `cargo test`, another user's build), it terminates no
process, and a holder's text is never authority to signal or reset anything. A target has one use marker as well; a
busy marker means wait, never delete.

### Pre-PR check

Run the CI set locally; it is the mandated check before a PR (status: standing rule, unchanged):

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --profile default --workspace   # the suite runner; nextest 0.9.152
cargo nextest run --profile alone --workspace     # the binary that needs the host to itself
cargo test --workspace --doc                      # nextest does not run doc-tests
cargo xtask crate-graph
cargo deny check          # only if cargo-deny is installed; CI always runs it
cargo xtask changelog check   # when the PR adds or edits a changelog fragment; CI does not run it yet
```

`./xtask/gate.sh` (the `full` tier, cold) is for releases, for the once-per-goal run and for changes to layering,
process handling or security-relevant code. It is not a superset of CI's `check`: without `cargo-deny` its `advisories`
step reports `ENV-LIMITED` and the run exits 3 instead of passing, so run `cargo deny check` yourself. `--warm` reuses
your `target/` and is not a gate. It runs on macOS and Linux.

Say in the PR which platform each check ran on (Linux CI, a Linux host, `Linux (Docker linux/amd64)`, or a
combination); through 2.0 a macOS result is neither required nor a gate (rule 10). A failure that also occurs on
the base commit is still a failure. Report it as one; it is not a pass.

### Linux checks on macOS (Docker)

On macOS, when Docker is available, you may run the Linux checks before you push with `cargo xtask linux`, in a
linux/amd64 container. It is optional: it does not replace the pre-PR check on your own platform, and Linux CI must
still be green. Without Docker, report the Linux checks as `env-limited` (Linux CI), as before.

```
cargo xtask linux                                # the CI set, in linux/amd64
cargo xtask linux run cargo test -p orch-infra   # one command in the container
cargo xtask linux shell                          # an interactive shell (needs a terminal)
cargo xtask linux clean                          # delete this checkout's target volume (--all: every orch-linux volume)
```

- **Image.** The official `rust:<channel>-trixie` (Debian 13), with `<channel>` read from `rust-toolchain.toml`, so
  the container runs the toolchain CI installs. The xtask adds tmux, rustfmt and clippy and tags the result
  `orch-linux:<channel>` locally; it is never pushed. The base already has the git, gcc, pkg-config and python3 the
  suite needs (the `slim` variant has no git, python3 or pkg-config). CI's jobs run in the same Debian 13 base
  (`rust:1-trixie`, pinned by digest in `ci.yml`, with the channel installed from `rust-toolchain.toml`), so the
  distribution (glibc 2.41) and tmux (3.5a) match; the user differs (your uid here, root in CI's job container).
- **What it runs.** CI's commands in CI's order: the `Cargo.lock` termwiz/terminfo check, `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo xtask crate-graph` and `cargo test --workspace
  --no-fail-fast`, stopping at the first failing step. Not cargo-deny: `deny.toml` already selects the Linux target,
  so run `cargo deny check` on the host. Not `./xtask/gate.sh` (see Platform behaviours below).
- **Setup.** Docker must be able to run linux/amd64 containers. On Apple silicon Docker emulates amd64; the xtask
  works with whichever emulator Docker uses, never configures or names one, and prints whether the run is native or
  emulated. Tested with 8 GiB of memory for Docker. A cold run uses about 8.6 GB of Docker's disk: 2.4 GB of image,
  6.0 GB of target volume and 0.2 GB of cargo registry.
- **Time.** On an M5 Max, with Docker Desktop's emulation as of 2026-11 (Rosetta-based), a run with empty target and
  registry volumes took 13 minutes (the image was already built; a first run also pulls the base image) and a warm
  one 10: under emulation the test suite itself takes about 10 minutes. Another emulator may be much slower. Agents
  run it in the background: a cold run takes more than ten minutes, and some agent tools stop a foreground command
  sooner. A narrower `run` (say `cargo test -p orch-infra`) can still rebuild some crates, because cargo resolves
  features for what it builds.
- **Isolation.** The checkout is mounted read-only at its own path, and so is a linked worktree's main git
  directory. The run tests your working tree as it is, uncommitted changes included, so do not edit files while it
  runs. `target/` is a Docker volume per checkout, so Linux artifacts never mix with the macOS ones in your
  `target/`, which gets only the macOS build of xtask itself; the cargo registry is one shared volume. The container
  gets no host environment and runs as your uid, not root, because some tests expect a read-only directory to refuse
  a write and root ignores that. There is one container per checkout (`orch-linux-<key>`): the xtask refuses to
  start a second, and warns when another checkout's is running, since two at once can exhaust Docker's memory.
- **Platform behaviours.** Two behaviours of an emulated platform, not of orch, break tests that pass on Linux:
  glibc's `posix_spawnp` of a missing binary returns a pid and the child exits 127 instead of failing with ENOENT,
  and starting any process writes into `$HOME` (an empty `.cache/rosetta`). Before the tests, the default run probes
  the container for each behaviour and skips, by exact name, only the tests that depend on what it finds
  (`PLATFORM_SKIPS` in `xtask/src/linux.rs`), printing what it found and what it skipped. Both were found under
  Docker Desktop's amd64 emulation in 2026-11 (7 tests skipped); neither shows in a native linux/arm64 container,
  and Linux CI runs all 7. Under another emulator the probes decide: a behaviour it lacks skips nothing, and a new
  one shows up as an ordinary failure (see Reporting). `run` neither probes nor skips. Where `$HOME` gets written,
  `./xtask/gate.sh` cannot pass in the container (its home-hygiene step, and its test step skips nothing): run the
  gate on native Linux; a macOS run is not required before 2.0 (rule 10).
- **Reporting.** The run prints the commit, the tree, whether the tree was dirty and the base image digest. In the
  PR, name the platform `Linux (Docker linux/amd64)`, never "Linux CI", with the commit it ran at, `dirty no`,
  whether the run was native or emulated, and the tests it skipped. A failure seen only in the container is still a
  failure: report it with the test name and re-run it once on the base commit, in a worktree of the base with `cargo
  xtask linux run cargo test … -- --exact <test>`. Call it a platform artifact only if it also fails there while
  Linux CI passes it on that base; then, in its own PR that cites that passing Linux CI run, add it to
  `PLATFORM_SKIPS` under the behaviour that causes it, with a new probe if no existing one covers it. A failure that
  mentions a read-only file system means a test or step writes into the checkout: report it, do not work around it
  (the full suite ran without one).
- **Exit status.** 0 when every step passed, 1 when one failed, the command's own status for `run`, and 2 when
  nothing ran: Docker missing or stopped, the image build failed (no network), or a toolchain pin that is not
  `X.Y.Z` (all `env-limited`), a container left for this checkout (a killed run leaves one; the message gives the
  `docker rm -f` command), or `shell` without a terminal.

### Broken tooling

If a repo script, test helper or fixture does not work on the platform you are using, fix it in its own small PR
(or report it to the lead), then continue. Never build an external harness, shim or runner around it. If a
check truly cannot run where you are, report it as `env-limited` and name where it must run (usually Linux CI).
On macOS with Docker the Linux checks can run locally; see [Linux checks on macOS
(Docker)](#linux-checks-on-macos-docker).

A test that fails during your work gets one re-run on the base commit. If it also fails there, it is
pre-existing: it gets its own fix or issue, and your PR states that. It does not reopen the review of your
change.

### Parallel agent work

Use a git worktree and a branch per agent: `git worktree add .worktrees/<name> -b
<branch>` (`.worktrees/` is git-excluded), or a sibling dir `../r-orch-<name>`. Parallel builders never share a
worktree. Commit early, and integrate by merging branches, not by passing patch files around. Never share a
`CARGO_TARGET_DIR` between worktrees (a harness once graded the wrong binary). Delete your throwaway target
dirs and worktrees when the work merges.

## Specs and plans

Goal-sized work follows **spec → plan → tasks → PRs** ([ADR 0004](docs/decisions/0004-spec-driven-development.md)).
The spec says what must be true and how it is checked. The plan says how it is built and splits it into tasks.
Each task lands as a PR whose verify command passes.

### Specs (`docs/specs/`)

Required when a plan is required **and** the work has behaviour a user or operator will notice. An owner
request that meets no plan trigger needs no spec. Read [docs/specs/README.md](docs/specs/README.md); start from
`docs/specs/TEMPLATE.md`.

- The owner's request is quoted verbatim, with numbered items. Every item maps to a requirement (`R1`, `R2`, ...)
  or to a `won't` row with a reason.
- Every requirement has an acceptance check: a named test or an exact command, and the platform it runs on.
- Fill in the security checklist for anything that listens on a socket, crosses a process boundary or handles
  untrusted text before building, not at the review gate.
- List the verification environment (what needs Linux, a second Linux host, a terminal other than tmux, a browser,
  owner permission) and ask about it in the first approval request. Through 2.0 the platform of every row is
  Linux (rule 10): no row waits for macOS, and a macOS-only gap is an issue, not an `env-limited` row.
  Terminal behaviour is verified in tmux (see TUI end-to-end tests).
- `status: draft -> approved -> done | superseded`. The owner approves the requirements before the build
  starts. The lead may approve them only where the owner's request says so; quote that line in the spec's
  Source. Review findings that are real requirements become new rows.
- The completing PR fills the spec's Verification table: result (`pass` / `env-limited` / `waived by owner`),
  the commit the check ran at, and the platform, for every row. A `must` row that is `env-limited` blocks
  `done` until it passes where it can run, or until the owner waives it.

### Plans (`docs/plans/`)

A plan is required when the change: alters an on-disk, CLI, `--json` or peer-protocol contract; adds a subsystem
or a load-bearing dependency; will take more than about a day; or the lead asks for one. Crossing crates alone
does not require a plan (the layered workspace makes that routine). Other fixes need none.

- File: `docs/plans/YYYY-MM-DD-<slug>.md` (same slug as its spec), from `docs/plans/TEMPLATE.md`.
- First line under the title: `status: proposed | active | done | abandoned`. Lifecycle:
  `proposed -> active -> done`, or `abandoned` with a one-line reason. Do not delete finished plans.
- About a page of design, then a **task table**. Each task row gives the requirements it covers, the paths in
  scope, its dependencies, a verify command and the PR it lands in. The task table is the PR split. Evidence
  goes in PRs, not in the plan.
- The plan lands before or in the same PR as the implementation. Never after the fact.
- When reality diverges from the plan, edit the plan in the PR that diverges. A stale plan is worse than none.
  Do not fork it into versions.
- Set `done` (plan and spec) in the PR that completes the last task.

### Design review loops

Bounded by what each round finds, not by a count. The lead decides whether another round runs, and from the
third round on writes the reason in the plan or escalates to the owner. A test settles what a test can settle,
and a walking skeleton comes before a second revision. The rules are in
[docs/plans/README.md](docs/plans/README.md#design-review).

### Handing out and finishing tasks

- **A task brief is self-contained.** It gives: links to the spec and plan, the task id and the requirement ids
  it covers, the paths in scope, the worktree and branch, the verify command, and what "done" means. Touching an
  adjacent file is allowed without a round-trip to the lead, as long as the PR still has one concern and its
  description names the file.
- **"Done" means evidence.** A task is done when a commit exists on its branch and its verify command passed
  there, both stated in the PR or handoff. Status in a tracker is not evidence. A lead releases dependent tasks
  only after checking that evidence.
- **Reviewers write the verdict skeleton first** (scope, what will be checked), then fill it in, so an
  interrupted review still leaves a report. A diff over about 800 lines is reviewed in slices.
- **Checkpoints change something.** A lead checkpoint must produce or change a deliverable (code, test, doc) or
  record a decision in the spec, the plan, an ADR or a PR. Reading or coverage ledgers are not gates.
- If a required reviewer is unavailable, use a substitute and record the substitution and why it is still
  independent. Do not retry the same failing reviewer again and again.

## Decisions (`docs/decisions/`)

ADR-lite: `docs/decisions/NNNN-<slug>.md`, four-digit sequential number, never reused or renumbered.

Write one for: an irreversible or cross-cutting choice; adopting or replacing a load-bearing dependency; a change
to an on-disk, CLI, `--json` or peer-protocol contract; any edit to `docs/TRAITS-FROZEN.md`; dropping a
requirement. Sections: Status / Context / Decision / Consequences. Accepted ADRs are not rewritten; supersede
them with a new one and mark the old `superseded by NNNN`. See [docs/decisions/README.md](docs/decisions/README.md).

`docs/reviews/` holds historical gate reviews from the early port-era work. Do not add new material there.

## Documentation duties

Docs ship in the same PR as the change. The PR template asks which row applied.

| If you change... | Update |
|---|---|
| User-facing behaviour, flags, install, upgrade or rollback steps | `README.md` |
| An operator-visible failure mode or a new error message | `docs/RUNBOOK.md` |
| Crate boundaries, dependency edges, traits, concurrency or error model | `docs/ARCHITECTURE.md` (and keep `cargo xtask crate-graph` passing) |
| Anything a user could notice | A changelog fragment in `docs/changelog/unreleased/` (never `CHANGELOG.md`; see below) |
| Material from outside the project (copied, adapted or vendored) | `THIRD-PARTY-NOTICES.md` (and `NOTICE` only when required; see Licensing) |
| A trait in `orch-core` listed in `docs/TRAITS-FROZEN.md` | Only via an ADR, then that file |
| A choice someone will later ask "why?" about | An ADR |
| What goal-sized work must do, and how it is accepted | A spec |
| Scope or sequencing of non-trivial work | A plan |
| A wire format, on-disk layout or protocol | An ADR, plus a reference doc under `docs/` in the same PR. Never only in an agent's scratch state |

### Changelog fragments

A release note is one file per entry, so PRs never edit the same lines. Pure refactors, tests and CI need no
entry. The byte-level contract is the [spec](docs/specs/2026-11-23-changelog-fragments.md); decision:
[ADR 0021](docs/decisions/0021-changelog-fragments.md).

1. Create `docs/changelog/unreleased/<slug>.<kind>.md` in the PR's first commit. `<slug>` is lowercase letters and
   digits joined by `-`, usually the branch slug. `<kind>` is `added`, `changed`, `deprecated`, `removed`, `fixed`
   or `security`. Two entries of one kind need two slugs.
2. The file is one line, user-voiced ("You can now ...", not "Refactored ..."), at most 1,000 bytes, starting with
   a letter or a backtick, ending in a newline. No PR number: it does not exist yet, and a commit to add it would
   change the head. No heading, bullet or link-definition syntax at the start of the line.
3. Do not edit `CHANGELOG.md` in a PR. Only the release PR does, through the assembler. `## [Unreleased]` still
   holds the hand-written notes from before fragments; leave them, the assembler carries them into the next
   release unchanged.
4. Run `cargo xtask changelog check` before pushing. It validates every fragment and changes nothing.

```
printf '%s\n' 'Fix the client socket timeout so a slow daemon reply no longer ends a command early.' \
  > docs/changelog/unreleased/socket-timeout.fixed.md
cargo xtask changelog check
```

An agent that runs the `document-release` skill applies its CHANGELOG step to the fragments the branch adds or
edits, never to `CHANGELOG.md`. The skill file under `skills/library/` ships to users' projects and stays generic.

Fragments sit under `docs/` on purpose: a fragment-only change classifies as documentation-only (`cargo xtask gate
select`), and a top-level directory would select the full suite.

## Rust coding standards

Rules a lint can check are enforced by lints. The rest are conventions that reviewers check.

**Enforced.** Every crate opts in with `[lints] workspace = true`. Do not add per-crate `[lints]` tables.

| Where | Lints | Configured in |
|---|---|---|
| Every crate and test target | `unsafe_code` forbidden; clippy `all`, `dbg_macro`, `todo`, `unimplemented` and `allow_attributes_without_reason` denied | `[workspace.lints]` in `Cargo.toml` |
| Library code (`orch-domain`, `orch-core`, `orch-infra`, `orch-engine`, `orch-peer`, `orch-tui`) | `unreachable_pub`, `clippy::unwrap_used`, `clippy::print_stdout`, `clippy::print_stderr` denied | `#![deny(...)]` at the top of each `lib.rs` |
| `orch-cli` library | `unreachable_pub` and `clippy::unwrap_used` denied; printing allowed, because it renders the CLI | `crates/orch-cli/src/lib.rs` |
| `#[cfg(test)]` code | `unwrap`, `print` and `dbg!` allowed | `clippy.toml` |

Integration tests (`crates/*/tests/`) and `xtask` get only the workspace lints. To allow a lint, write
`#[allow(lint, reason = "...")]` on the smallest item that needs it. The reason states a fact, not "needed".
Never allow a lint at a crate root.

**Conventions.**
- **Errors.** Follow ARCHITECTURE §5.7: libraries return typed `thiserror` enums and never `anyhow`, which is
  for `orch-cli` (whose top-level handler is §5.7's single exit point) and `xtask` only. Add context where an error crosses a boundary (the path, the peer, the task
  id), and propagate with `?`.
- **No panics on input.** Data from disk, the network, a child process or the user is validated and bounded
  (size, count, depth), never indexed or unwrapped. `expect("<why this cannot fail>")` is for local invariants
  only. For a `Mutex` that guards plain data, recover from poisoning with
  `.lock().unwrap_or_else(PoisonError::into_inner)`.
- **Async.** No blocking calls inside `async` code: no `std::fs`, no `std::thread::sleep`, no sync lock held
  across `.await`. Use tokio's equivalents or `spawn_blocking`. Every remote call has a timeout. Spawned tasks
  are owned (a `JoinSet` or a kept handle) and stopped on shutdown, never fire-and-forget.
- **Visibility.** Make items `pub(crate)` unless another crate uses them. The traits in `orch-core` change only
  as `docs/TRAITS-FROZEN.md` says.
- **Types over strings.** IDs, statuses and modes are enums or newtypes. Serialized field names that are part of
  an on-disk or `--json` contract are pinned with `#[serde(rename = "...")]`, so a Rust rename cannot change
  the contract.
- **Comments** say why, not what. Public items in library crates should have a doc comment (a convention;
  `missing_docs` is not enforced).
- **Formatting** is `rustfmt` defaults (there is no `rustfmt.toml`); do not add style options.

## Repository layout

```
crates/<crate>/
  Cargo.toml          # [lints] workspace = true; dependencies only via [workspace.dependencies]
  src/lib.rs          # crate docs, lint attributes, module declarations, re-exports; no logic
  src/<feature>.rs    # a module ...
  src/<feature>/      # ... and its children (not <feature>/mod.rs for new modules)
  tests/<feature>.rs  # integration tests, one file per feature or scenario group
  tests/common/mod.rs # shared test helpers (tests/<dir>/mod.rs is how Cargo shares code between test files)
fixtures/<kind>/      # recorded and scripted inputs shared across crates
skills/               # the skill library shipped with orch
xtask/                # dev tooling, run as `cargo xtask <cmd>`
docs/                 # ARCHITECTURE, RUNBOOK, TRAITS-FROZEN, specs/, plans/, decisions/,
                      # changelog/unreleased/ (one fragment per release note)
                      # (reviews/ is historical)
```

- The crate dependency direction is ARCHITECTURE §3.1, and `cargo xtask crate-graph` enforces it. The module
  map is §3.3; update it when you add or move a top-level module.
- Group modules by feature or domain (`peer/`, `messages/`, `wizard/`). Do not create kind-based grab-bags
  (`utils.rs`, `helpers/`, `types.rs`).
- Unit tests go in an inline `#[cfg(test)] mod tests` at the end of the file they test. Test-only helpers inside
  `src/` go in a `#[cfg(test)]` module.
- New modules use `<feature>.rs` plus `<feature>/`. Convert an existing `mod.rs` only in a PR that already
  changes that module.
- Scripts that will be run again are `xtask` subcommands in Rust, not shell scripts, so they behave the same on
  macOS and Linux.
- Documentation lives in `docs/` or in a crate's `//!` docs. Do not add new Markdown files inside `crates/`.
  The existing ones (`TEST-MAP.md`, `TEST-COVERAGE.md`, `BACKEND.md`) predate this rule.

## Testing

- **Unit tests:** inline `#[cfg(test)]` modules.
- **Integration tests:** `crates/<crate>/tests/`. CLI tests drive the real binary with `assert_cmd`
  (`crates/orch-cli/tests/`). Engine tests use the fakes in `crates/orch-engine/tests/common`.
- **TUI:** `orch-tui` tests render against an in-crate `FakeBackend` (`crates/orch-tui/tests/`); dashboard flows run in `crates/orch-cli/tests/tui_*.rs`.
  What an operator sees in a real terminal is covered by end-to-end tests in tmux (below).
- **Snapshots:** `insta` is used for adapter argv (`crates/orch-infra/tests/adapter_argv.rs`); review with `cargo insta review`, never accept blindly.
- **Adapters:** unit tests inject a scripted `FakeProcessManager` and assert exact argv and stdin. Recorded
  adapter output is under `fixtures/adapters/<cli>/`.
- **Shell adapter:** when a test needs an agent that really runs, use the `shell` adapter against a script you
  write into a temp dir. Fake agent scripts are in `fixtures/fake-agents/`.
- **No real AI CLIs in tests.** Never exec `claude`, `codex`, `cursor-agent`, etc., and never read their config
  dirs. The gate's `no-real-cli` step puts shims on `PATH` and fails if any test reaches one.
- **Run one crate:** `cargo test -p orch-peer`; one test: `cargo test -p orch-cli --test remote_peers <name>`. Under
  nextest: `cargo nextest run -p orch-peer`, and `cargo nextest run -E 'test(<name>)'` for one test. A test that needs
  isolation is listed in `.config/nextest.toml`, never serialised by a lock in the test file (see "Parallel test
  binaries").
- **Portable by default.** Write tests, fixtures and scripts to run on macOS and Linux, or mark them Linux-only
  (and then CI runs them). No GNU-only flags (`sed -i` without a suffix argument, `df -BG`, `date -Is`); no
  `/proc` outside `#[cfg(target_os = "linux")]`. Through 2.0 only Linux is run and gated (rule 10): a test that
  fails only on macOS is filed in the [After 2.0](https://github.com/yuxhuang/r-orch/milestone/20) milestone and
  does not block a PR; it is not a "baseline" that excuses a Linux failure, and a failure that Linux shows too is
  a bug to fix.
- **Hermetic child processes.** A test that spawns a binary gives it its own temp root and a cleared or explicit
  environment. Runner variables (`CARGO_TARGET_DIR`, `TMPDIR`, wrapper `PATH`s) must not leak in. Keep
  scenario-heavy test files split by scenario group, so parallel work does not serialise on one file.

### TUI end-to-end tests (tmux)

The dashboard is tested at three levels:

| Level | Where | Proves |
|---|---|---|
| Render and state | `crates/orch-tui/tests/`, `FakeBackend` | Layout, key handling, state transitions |
| Dashboard flow | `crates/orch-cli/tests/tui_dashboard.rs` (ratatui `TestBackend`, real engine) | The TUI and the engine agree |
| End to end | The real `orch` binary running in tmux | What an operator sees and does in a real terminal |

A requirement about keys, mouse, focus, scrolling, resize, copy or anything else on screen is not `pass` until an
end-to-end test covers it. Tests against `FakeBackend` or `TestBackend` alone do not count. Process-level tty
behaviour (hang-up, SIGHUP) stays on `portable-pty`, as in `crates/orch-cli/tests/tui_hangup.rs`.

**Use tmux as the terminal**, both for automated tests and for manual or agent acceptance runs:

- **Isolated server.** Run every command as `tmux -L <unique-socket> -f /dev/null ...`. That gives each test its
  own server and no user config. Never attach to or touch the developer's tmux server. Run `kill-server` when
  the test ends, including on failure (use a drop guard).
- **Hermetic session.** Run the dashboard in a temp project, and pin the terminal type before the session
  exists, so key encodings do not depend on the host:
  `tmux -L <sock> -f /dev/null start-server \; set-option -g default-terminal tmux-256color \; new-session -d
  -s <name> -x 120 -y 40 -e VAR=value '<orch binary> tui'`. Pass the environment the binary needs with `-e`,
  including `ORCHESTRY_GLOBAL_CONFIG_PATH`.
- **Input.**
  - Named keys: `send-keys -t <name> Down PageDown Home End Enter Escape Tab`.
  - Literal text: `send-keys -t <name> -l '<text>'`.
  - Raw bytes, such as SGR mouse events: `send-keys -t <name> -H 1b 5b 3c 30 3b 31 30 3b 35 4d` is
    `ESC[<0;10;5M`, a left-button press at column 10, row 5.
- **Output.** `capture-pane -p -t <name>` returns the screen as text; add `-e` when the test asserts colours or
  attributes. Poll until the expected text appears, with a bounded deadline. Never use a fixed sleep as the
  assertion.
- **Resize.** `resize-window -t <name> -x 80 -y 24`, then assert the redrawn screen.
- **Clipboard.** After `set-option -s set-clipboard on`, an OSC 52 write from the dashboard lands in tmux's paste
  buffer. Read it back with `show-buffer`. This checks the terminal copy route without touching the system
  clipboard.
- **Not covered by tmux:** native terminal profiles (Terminal.app, iTerm2), the native system clipboard, and
  `orch web` in a browser. List these in the spec's Verification environment. Record them as `env-limited` or
  `waived by owner` if they are not run.
- **Installing tmux.** Use `brew install tmux` on macOS and the distro package on Linux. CI does not install
  tmux yet, so the first PR that adds a tmux test also adds the install step to `.github/workflows/ci.yml`. A
  tmux test must fail with a clear message when tmux is missing, never pass silently.
- **Manual and agent acceptance** use the same commands against a temp project, never a real `.orchestry`.
  Paste the `capture-pane` output into the PR as evidence.

## Release

1. Branch `chore/release-vX.Y.Z` from `main`. Do not use `release/vX.Y.Z`: the ruleset protects
   `release/**`. Run the cold gate (`./xtask/gate.sh full`) on Linux, the only platform required through 2.0 (see Verification tiers and rule 10). Every
   gate step must pass and the result must say `qualifying: yes`. Record the platform, the commit and the result in the PR.
2. Set `workspace.package.version` in `Cargo.toml` (and let `Cargo.lock` update).
3. Assemble the release notes from the repository root, after step 2 (the version must equal `Cargo.toml`):
   `cargo xtask changelog assemble --version X.Y.Z --date YYYY-MM-DD`. Add `--dry-run` first to print the section
   without writing. The command renames `## [Unreleased]` to `## [X.Y.Z] - YYYY-MM-DD` and adds a fresh empty
   `## [Unreleased]`. Under the new heading it puts the existing Unreleased text unchanged, then one `###` section
   per kind from the fragments (Added, Changed, Deprecated, Removed, Fixed, Security, entries sorted by file
   name). It repoints `[Unreleased]:` and adds `[X.Y.Z]:` as compare links, and deletes the fragments it consumed.
   It refuses, changing nothing, on a malformed fragment, a version that is not newer than the last release or
   does not match `Cargo.toml`, or nothing to release. Then edit the section by hand where needed: merge the
   two `### Added` headings that the first assembly after the cutover can produce, and add the Linux-only
   statement of step 4. Do not edit `CHANGELOG.md` by hand before assembling.
4. Update README release notes (the "Upgrading" and "Known gaps" sections) if behaviour users must act on changed.
   For every release from 1.1 through 2.0, whatever else changed, the README upgrade notes, the `CHANGELOG.md`
   entry and the release PR each state that the release was qualified on Linux only and that macOS is untested
   until after 2.0 (rule 10). Packaging, the Homebrew formula and the workflow are not changed for it.
5. Commit `chore(release): X.Y.Z` with `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md` and the deleted fragments
   together, open the PR, CI green, owner merges (merge commit, per the convention).
6. On `main`: `git tag -a vX.Y.Z -m "vX.Y.Z"` then `git push origin vX.Y.Z`.
7. Create the GitHub release with notes copied from the CHANGELOG section:
   `gh release create vX.Y.Z --title vX.Y.Z --notes-file <extract>`.
8. The tag push runs `.github/workflows/homebrew.yml`, which points the Homebrew formula at the new tag and
   commit. It skips pre-release tags, and refuses a tag that is not on `main` or does not match `Cargo.toml`.
   Check that the run is green. To retry it, or for a tag created before the workflow existed, run
   `gh workflow run homebrew -f tag=vX.Y.Z`. The tap's name and token live in repository variables and secrets
   (see the workflow's header), never in the repo.

`orch update` does not self-update; it only prints install instructions, so there is no update channel to
publish. Never tag from a branch other than `main`, and never move or delete a pushed tag.

The release procedure is this section. Do not write a new release runbook or one-off publish scripts for each
release. If a step is missing, fix this section in the release PR. Scripts that are needed again belong under
`xtask/`. They must stop on any failed command and must take the version as a parameter. Agent sessions may have
no SSH agent, so use an HTTPS remote with the `gh` credential helper for pushes, and put HTTPS (or `<host>`)
URLs in docs. Apart from the formula update in step 8, downstream packaging lives outside this repo and is the
owner's to update.

## Roles

**Lead / orchestrator.** Reads: this file, open PRs, `docs/specs/`, `docs/plans/`. Owns: turning the owner's
request into a spec, scope, splitting work into one-concern PRs through the plan's task table, deciding plan and
ADR need, merging when delegated. Must not skip: getting the spec approved and writing or approving the plan
before work that needs them starts; self-contained task briefs (see Handing out and finishing tasks); checking
the evidence that a task is done before releasing the tasks that depend on it.

**Implementer.** Reads: the task brief, the spec and plan, the crate's ARCHITECTURE section, neighbouring tests.
Owns: code, tests and the doc updates from the duties table, in one PR. Must not skip: the green commands, the
changelog fragment, a test that fails without the change, the commit and verify result in the PR, and `fast` after
each round.

**Reviewer.** Read-only: does not edit, push or merge. Reads: the PR description, the diff, and the spec, plan
and ADRs it cites. Owns: a findings list with severity (blocker / should-fix / nit). Must not skip: stating
what was checked (commands run, files read, platform, what was not checked); checking the Don'ts below; checking
that the spec and plan status lines are correct. A finding that is a missing requirement goes into the spec as a
new row.

**Tech writer.** Reads: the code and tests being documented, existing docs. Owns: README, RUNBOOK, ARCHITECTURE,
changelog fragment wording, plans and ADR clarity. Must not skip: verifying every command and message against the code.
Document current behaviour only; no aspirational features.

**Release.** Reads: the Release section, `CHANGELOG.md`, `git log <last-tag>..HEAD`. Owns: version bump, changelog
assembly, tag, GitHub release. Must not skip: the cold gate, tagging only from `main`, asking the owner before
pushing a tag.

## Don'ts

- Don't use `sort_unstable*`; clippy will fail.
- Don't bump `rust-toolchain.toml` without bumping `rust-version` in `Cargo.toml` to match and running the CI set
  under the new toolchain; new clippy lints are fixed in the same PR, not allowed.
- Don't run tests or experiments against the real home directory or a real project's `.orchestry`.
- Don't change the repo's visibility, add remotes, or paste private names (taps, hosts, paths) anywhere in the repo or PRs.
- Don't add a dependency without the §4.8 check; add an ADR when it is load-bearing.
- Don't edit `docs/TRAITS-FROZEN.md` without an ADR.
- Don't push to `main`, force-push shared branches, or merge your own PR.
- Don't silence a lint without `reason = "..."` on the `#[allow]` (clippy enforces this), and don't skip hooks.
- Don't wrap a broken repo script in an external harness; fix the script (see Broken tooling).
- Don't create `release/**` branches; the ruleset protects them.
- Don't edit `CHANGELOG.md` outside a release PR; write a changelog fragment.
- Don't report a check as passed that did not run, ran only on another platform, or fails on the base commit too.
