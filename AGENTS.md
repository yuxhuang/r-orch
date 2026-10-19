# AGENTS.md

Read this first, whatever your role. Reasons live in the linked docs.

## What this is

r-orch is a Rust AI-agent orchestrator: a CLI (`orch`), a terminal dashboard and a runtime that drives external
AI coding CLIs as child processes and keeps all state as plain files under `.orchestry/`. It began as a port of
the TypeScript `oxgeneral/ORCH` and has since diverged (see [ADR 0002](docs/decisions/0002-parity-is-no-longer-a-goal.md)).
The repo is **private**, being prepared for open source.

| Crate | Role |
|---|---|
| `orch-domain` | Pure types and functions. No I/O, no async. |
| `orch-core` | Port traits: storage, adapter, process, workspace, clock, ids. |
| `orch-infra` | Implementations of the `orch-core` traits (files, adapters, processes). |
| `orch-engine` | Services, orchestrator, event bus, container. |
| `orch-peer` | Remote-peer protocol: links, mirrors, reconnect. |
| `orch-cli` | clap command tree, human and `--json` rendering, serve mode, `orch web`. Binaries live here. |
| `orch-tui` | ratatui dashboard. |
| `xtask` | Dev tooling: crate-graph, lint-scenarios, parity harness. Run as `cargo xtask <cmd>`. |

Dependency direction is asserted by `cargo xtask crate-graph` (ARCHITECTURE §3.1).
Authoritative docs:

| Question | Read |
|---|---|
| How is it built, which crate owns what, which traits | [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) |
| Something is broken at runtime | [docs/RUNBOOK.md](docs/RUNBOOK.md) |
| What users see and how they install | [README.md](README.md) |
| How to use every feature, step by step | [docs/manual/](docs/manual/README.md) (regenerate the command reference and screenshots with `docs/manual/tools/`) |
| What changed per release | [CHANGELOG.md](CHANGELOG.md) |
| Why we chose X | [docs/decisions/](docs/decisions/) |
| What a piece of work must do, and how we check it | [docs/specs/](docs/specs/) |
| How and in what order we will build it | [docs/plans/](docs/plans/) |
| Frozen trait signatures | [docs/TRAITS-FROZEN.md](docs/TRAITS-FROZEN.md) |
| Historical: the old parity contract and port reviews | `docs/PARITY.md`, `docs/parity/`, `docs/reviews/` |

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
7. **The repo stays private.** Do not change visibility, publish crates (`publish = false` is deliberate), or
   push to any other remote. Open-sourcing is the owner's decision.
8. **Parity is not a constraint.** New behaviour may diverge from the TypeScript original. See
   [ADR 0002](docs/decisions/0002-parity-is-no-longer-a-goal.md).
9. **Advisories go to the owner.** A new RUSTSEC advisory that reddens CI (the weekly scheduled run exists to
   surface this) is escalated, never silenced by the agent. A `deny.toml` `ignore` entry needs owner sign-off
   and the reasoned-comment format of the two existing entries: why no replacement, what bounds the exposure.
   Until `SECURITY.md` exists, report a vulnerability you find to the owner privately (GitHub private
   vulnerability reporting or direct message), never in a public issue, PR title or PR body.

## Workflow

Everything reaches `master` through a PR. A GitHub ruleset enforces this, needs 0 approvals, and requires the
`check` and `test` CI jobs to pass. The same ruleset protects `release/**`, so do not create branches under that
prefix. A red PR is not reviewed or merged.

1. Branch from current `master`: `fix/<slug>`, `feature/<slug>`, `docs/<slug>`, `chore/<slug>`, and
   `chore/release-vX.Y.Z` for releases.
2. Commit as `type(scope): summary`, imperative, no trailing period. Types in use: `feat`, `fix`, `docs`,
   `test`, `chore`, `refactor`. Scope is a crate or area: `fix(peer): bound a worker's free-form error text`,
   `chore(release): 0.5.1`. The summary says what changed in behaviour. Process labels ("integrate verified v6
   b0 slice") do not belong there.
3. Run the pre-PR check (below) before opening the PR.
4. Open the PR using `.github/PULL_REQUEST_TEMPLATE.md`; fill every section, delete none. One concern per PR;
   if the title needs "and", split it. A PR over about 3,000 changed lines is split, or the plan explains why
   it cannot be.
5. CI (`.github/workflows/ci.yml`, Linux only) must be green: fmt, clippy `-D warnings`, test, `cargo xtask
   crate-graph`, cargo-deny.
6. Merge, per the policy below. Implementers and reviewers never merge their own work.
   - Convention: squash for single-concern PRs (the squash title becomes the commit; make it a good one);
     merge commit for release PRs and multi-commit PRs whose commits are individually meaningful.
   - Never force-push `master`.

**Merge policy.** The human owner merges by default. The owner may standing-delegate categories to the
orchestrator; current delegation: docs-only, test-only, and CHANGELOG/plan-status PRs, once required checks are
green. The exception is a change to the process itself (`AGENTS.md`, `docs/decisions/`, and the `docs/specs/` or
`docs/plans/` README and TEMPLATE files), which the owner merges. Everything else waits for the owner. Changes to
the delegation are recorded here, in this paragraph.

### Pre-PR check

Run the CI set locally; it is the mandated check:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo xtask crate-graph
cargo deny check          # only if cargo-deny is installed; CI always runs it
```

`./xtask/gate.sh` (cold, needs ~12 GB free) is for releases and for changes to layering, process handling or
security-relevant code. It is not a superset of CI: its `advisories` step silently passes when cargo-deny is
missing. It also runs `parity`, `lint-scenarios` and `probes`, which need the TS oracle (`./parity/setup.sh`,
Node) and fail without it (every probe reports HARNESS BROKE); `probes` also fails on a dirty tree. Without the
oracle, expect those steps to fail and say so in the PR; do not contort code (ADR 0002). `--warm` reuses your
`target/` and is not a gate. Its verdict is written to `target/gate-result.txt`. It runs on macOS and Linux.

Say in the PR which platform each check ran on (macOS, Linux CI, both). A failure that also occurs on the base
commit is still a failure. Report it as one; it is not a pass.

### Broken tooling

If a repo script, test helper or fixture does not work on the platform you are using, fix it in its own small PR
(or report it to the lead), then continue. Never build an external harness, shim or runner around it. If a
check truly cannot run where you are, report it as `env-limited` and name where it must run (usually Linux CI).

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
- List the verification environment (what needs Linux, a second host, a terminal other than tmux, a browser,
  owner permission) and ask about it in the first approval request. Terminal behaviour is verified in tmux (see
  TUI end-to-end tests).
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

`docs/reviews/` holds historical gate reviews from the port. Do not add new material there.

## Documentation duties

Docs ship in the same PR as the change. The PR template asks which row applied.

| If you change... | Update |
|---|---|
| User-facing behaviour, flags, install, upgrade or rollback steps | `README.md` |
| An operator-visible failure mode or a new error message | `docs/RUNBOOK.md` |
| Crate boundaries, dependency edges, traits, concurrency or error model | `docs/ARCHITECTURE.md` (and keep `cargo xtask crate-graph` passing) |
| Anything a user could notice | `CHANGELOG.md`, under `## [Unreleased]` |
| A trait in `orch-core` listed in `docs/TRAITS-FROZEN.md` | Only via an ADR, then that file |
| A choice someone will later ask "why?" about | An ADR |
| What goal-sized work must do, and how it is accepted | A spec |
| Scope or sequencing of non-trivial work | A plan |
| A wire format, on-disk layout or protocol | An ADR, plus a reference doc under `docs/` in the same PR. Never only in an agent's scratch state |

CHANGELOG entries are one line, user-voiced, under Added / Changed / Deprecated / Removed / Fixed / Security.
Pure refactors, tests and CI need no entry.

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
docs/                 # ARCHITECTURE, RUNBOOK, TRAITS-FROZEN, specs/, plans/, decisions/
                      # (PARITY.md, parity/ and reviews/ are historical)
parity/               # the historical TypeScript oracle setup (see Testing)
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
  The existing ones (`TEST-MAP*.md`, `TEST-COVERAGE.md`, `BACKEND.md`) are left over from the port.

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
- **Cross-binary tests** (`orch-infra` `concurrent_index`, `cross_compat`) need the vendored oracle from
  `./parity/setup.sh`; without it set `ORCH_SKIP_CROSS_BINARY=1` to skip them, as CI does.
- **Differential parity harness** (`cargo xtask parity`, `fixtures/scenarios`): optional and historical. Keep
  the scenarios green where cheap; do not extend the harness for new features.
- **Run one crate:** `cargo test -p orch-peer`; one test: `cargo test -p orch-cli --test remote_peers <name>`.
- **Portable by default.** Tests, fixtures and scripts run on macOS and Linux, or are marked Linux-only (and
  then CI runs them). No GNU-only flags (`sed -i` without a suffix argument, `df -BG`, `date -Is`); no
  `/proc` outside `#[cfg(target_os = "linux")]`. A test that fails on macOS is a bug to fix, not a "baseline".
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

1. Branch `chore/release-vX.Y.Z` from `master`. Do not use `release/vX.Y.Z`: the ruleset protects
   `release/**`. Run the cold gate on a platform it supports (see Pre-PR check). The TS oracle is optional
   ([ADR 0002](docs/decisions/0002-parity-is-no-longer-a-goal.md)): without `./parity/setup.sh`, run
   `ORCH_SKIP_CROSS_BINARY=1 ./xtask/gate.sh`. The `lint-scenarios`, `parity` and `probes` steps need the
   oracle and do not block a release; every other step must pass. Record the platform and the result of those
   three steps in the PR.
2. Set `workspace.package.version` in `Cargo.toml` (and let `Cargo.lock` update).
3. In `CHANGELOG.md` rename `## [Unreleased]` to `## [X.Y.Z] - YYYY-MM-DD`, add a fresh empty `## [Unreleased]`,
   and update the compare links at the bottom.
4. Update README release notes (the "Upgrading" and "Known gaps" sections) if behaviour users must act on changed.
5. Commit `chore(release): X.Y.Z`, open the PR, CI green, owner merges (merge commit, per the convention).
6. On `master`: `git tag -a vX.Y.Z -m "vX.Y.Z"` then `git push origin vX.Y.Z`.
7. Create the GitHub release with notes copied from the CHANGELOG section:
   `gh release create vX.Y.Z --title vX.Y.Z --notes-file <extract>`.
8. The tag push runs `.github/workflows/homebrew.yml`, which points the Homebrew formula at the new tag and
   commit. It skips pre-release tags, and refuses a tag that is not on `master` or does not match `Cargo.toml`.
   Check that the run is green. To retry it, or for a tag created before the workflow existed, run
   `gh workflow run homebrew -f tag=vX.Y.Z`. The tap's name and token live in repository variables and secrets
   (see the workflow's header), never in the repo.

`orch update` does not self-update; it only prints install instructions, so there is no update channel to
publish. Never tag from a branch other than `master`, and never move or delete a pushed tag.

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
CHANGELOG line, a test that fails without the change, the commit and verify result in the PR.

**Reviewer.** Read-only: does not edit, push or merge. Reads: the PR description, the diff, and the spec, plan
and ADRs it cites. Owns: a findings list with severity (blocker / should-fix / nit). Must not skip: stating
what was checked (commands run, files read, platform, what was not checked); checking the Don'ts below; checking
that the spec and plan status lines are correct. A finding that is a missing requirement goes into the spec as a
new row. Do not reject for parity divergence.

**Tech writer.** Reads: the code and tests being documented, existing docs. Owns: README, RUNBOOK, ARCHITECTURE,
CHANGELOG wording, plans and ADR clarity. Must not skip: verifying every command and message against the code.
Document current behaviour only; no aspirational features.

**Release.** Reads: the Release section, `CHANGELOG.md`, `git log <last-tag>..HEAD`. Owns: version bump, changelog
cut, tag, GitHub release. Must not skip: the cold gate, tagging only from `master`, asking the owner before
pushing a tag.

## Don'ts

- Don't commit `parity/orch-ts/`; it is gitignored and re-created by `./parity/setup.sh`.
- Don't add PARITY.md §14 entries or extend the contract; leave existing `PARITY` code comments alone.
- Don't use `sort_unstable*`; clippy will fail.
- Don't bump `rust-toolchain.toml` without bumping `rust-version` in `Cargo.toml` to match and running the CI set
  under the new toolchain; new clippy lints are fixed in the same PR, not allowed.
- Don't run tests or experiments against the real home directory or a real project's `.orchestry`.
- Don't flip the repo public, add remotes, or paste private names (taps, hosts, paths) anywhere in the repo or PRs.
- Don't add a dependency without the §4.8 check; add an ADR when it is load-bearing.
- Don't edit `docs/TRAITS-FROZEN.md` without an ADR.
- Don't push to `master`, force-push shared branches, or merge your own PR.
- Don't silence a lint without `reason = "..."` on the `#[allow]` (clippy enforces this), and don't skip hooks.
- Don't wrap a broken repo script in an external harness; fix the script (see Broken tooling).
- Don't create `release/**` branches; the ruleset protects them.
- Don't report a check as passed that did not run, ran only on another platform, or fails on the base commit too.
