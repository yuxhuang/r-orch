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
| What changed per release | [CHANGELOG.md](CHANGELOG.md) |
| Why we chose X | [docs/decisions/](docs/decisions/) |
| What we are about to build | [docs/plans/](docs/plans/) |
| Frozen trait signatures | [docs/TRAITS-FROZEN.md](docs/TRAITS-FROZEN.md) |
| Historical: the old parity contract and port reviews | `docs/PARITY.md`, `docs/parity/`, `docs/reviews/` |

## Non-negotiables

1. **Pinned toolchain.** `rust-toolchain.toml` pins 1.94.1 (edition 2024). Do not override it or bump it casually
   (see Don'ts). Only rustup honours the pin: if `rustup which cargo` fails, your `cargo` (e.g. Homebrew) ignores
   it and your clippy/fmt results will differ from CI. Match CI (install rustup).
2. **Green before a PR:** the CI set, run locally (see Pre-PR check). A red PR is not reviewed or merged.
3. **Stable sorts only.** `clippy.toml` bans `sort_unstable`, `sort_unstable_by`, `sort_unstable_by_key`. Index
   order decides which agent is picked, so stability is behaviour. Where a sort is provably not over index
   order and speed matters, use `#[allow(clippy::disallowed_methods)]` with a one-line reason.
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

Everything reaches `master` through a PR (a GitHub ruleset enforces it; 0 approvals required). The `ci / check`
and `ci / test` jobs are to become required status checks on `master` (the owner enables them after this setup's
first green run; see the open-source-readiness plan). Until then CI green is on the author. A red PR is not
reviewed or merged.

1. Branch from current `master`: `fix/<slug>`, `feature/<slug>`, `docs/<slug>`, `release/vX.Y.Z`.
2. Commit as `type(scope): summary`, imperative, no trailing period. Types in use: `feat`, `fix`, `docs`,
   `test`, `chore`, `refactor`. Scope is a crate or area: `fix(peer): bound a worker's free-form error text`,
   `chore(release): 0.5.1`.
3. Run the pre-PR check (below) before opening the PR.
4. Open the PR using `.github/PULL_REQUEST_TEMPLATE.md`; fill every section, delete none. One concern per PR;
   if the title needs "and", split it.
5. CI (`.github/workflows/ci.yml`, Linux only) must be green: fmt, clippy `-D warnings`, test, `cargo xtask
   crate-graph`, cargo-deny.
6. Merge, per the policy below. Implementers and reviewers never merge their own work.
   - Convention: squash for single-concern PRs (the squash title becomes the commit; make it a good one);
     merge commit for release PRs and multi-commit PRs whose commits are individually meaningful.
   - Never force-push `master`.

**Merge policy.** The human owner merges by default. The owner may standing-delegate categories to the
orchestrator; current delegation: docs-only, test-only, and CHANGELOG/plan-status PRs, once required checks are
green. Everything else waits for the owner. Changes to the delegation are recorded here, in this paragraph.

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
`target/` and is not a gate. Its verdict is written to `target/gate-result.txt`.

### Parallel agent work

Use a git worktree per agent: `git worktree add .worktrees/<name> -b
<branch>` (`.worktrees/` is git-excluded), or a sibling dir `../r-orch-<name>`. Never share a
`CARGO_TARGET_DIR` between worktrees (a harness once graded the wrong binary).

## Plans (`docs/plans/`)

A plan is required when the change: alters an on-disk, CLI, `--json` or peer-protocol contract; adds a subsystem
or a load-bearing dependency; will take more than about a day; or the lead asks for one. Crossing crates alone
does not require a plan (the layered workspace makes that routine). Other fixes need none.

- File: `docs/plans/YYYY-MM-DD-<slug>.md`, from `docs/plans/TEMPLATE.md`.
- First line under the title: `status: proposed | active | done | abandoned`. Lifecycle:
  `proposed -> active -> done`, or `abandoned` with a one-line reason. Do not delete finished plans.
- The plan lands before or in the same PR as the implementation. Never after the fact.
- When reality diverges from the plan, edit the plan in the PR that diverges. A stale plan is worse than none.
- Set `done` in the PR that completes the last checklist item.

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
| Scope or sequencing of non-trivial work | A plan |

CHANGELOG entries are one line, user-voiced, under Added / Changed / Deprecated / Removed / Fixed / Security.
Pure refactors, tests and CI need no entry.

## Testing

- **Unit tests:** inline `#[cfg(test)]` modules.
- **Integration tests:** `crates/<crate>/tests/`. CLI tests drive the real binary with `assert_cmd`
  (`crates/orch-cli/tests/`). Engine tests use the fakes in `crates/orch-engine/tests/common`.
- **TUI:** `orch-tui` tests render against an in-crate `FakeBackend` (`crates/orch-tui/tests/`); dashboard flows run in `crates/orch-cli/tests/tui_*.rs`.
- **Snapshots:** `insta` is used for adapter argv (`crates/orch-infra/tests/adapter_argv.rs`); review with `cargo insta review`, never accept blindly.
- **Adapters:** unit tests inject a scripted `FakeProcessManager` and assert exact argv and stdin. Recorded
  adapter output is under `fixtures/adapters/<cli>/`.
- **Shell adapter:** when a test needs an agent that really runs, use the `shell` adapter against a script you
  write into a temp dir. Fake agent scripts are in `fixtures/fake-agents/`.
- **No real AI CLIs in tests.** Never exec `claude`, `codex`, `cursor-agent`, etc., and never read their config
  dirs. The gate's `no-real-cli` step puts shims on `PATH` and fails if any test reaches one.
- **Differential parity harness** (`cargo xtask parity`, `fixtures/scenarios`): optional and historical. Keep
  the scenarios green where cheap; do not extend the harness for new features.
- **Run one crate:** `cargo test -p orch-peer`; one test: `cargo test -p orch-cli --test remote_peers <name>`.

## Release

1. Branch `release/vX.Y.Z` from `master`. Run the cold `./xtask/gate.sh` (after `./parity/setup.sh`); it must pass.
2. Set `workspace.package.version` in `Cargo.toml` (and let `Cargo.lock` update).
3. In `CHANGELOG.md` rename `## [Unreleased]` to `## [X.Y.Z] - YYYY-MM-DD`, add a fresh empty `## [Unreleased]`,
   and update the compare links at the bottom.
4. Update README release notes (the "Upgrading" and "Known gaps" sections) if behaviour users must act on changed.
5. Commit `chore(release): X.Y.Z`, open the PR, CI green, owner merges (merge commit, per the convention).
6. On `master`: `git tag -a vX.Y.Z -m "vX.Y.Z"` then `git push origin vX.Y.Z`.
7. Create the GitHub release with notes copied from the CHANGELOG section:
   `gh release create vX.Y.Z --title vX.Y.Z --notes-file <extract>`.

`orch update` does not self-update; it only prints install instructions, so there is no update channel to
publish. Never tag from a branch other than `master`, and never move or delete a pushed tag.

## Roles

**Lead / orchestrator.** Reads: this file, open PRs, `docs/plans/`. Owns: scope, splitting work into one-concern
PRs, deciding plan and ADR need, merging when delegated. Must not skip: writing or approving the plan before
work that needs a plan starts; telling implementers which worktree and branch to use.

**Implementer.** Reads: the plan, the crate's ARCHITECTURE section, neighbouring tests. Owns: code, tests and the
doc updates from the duties table, in one PR. Must not skip: the green commands, the CHANGELOG line, a test that
fails without the change.

**Reviewer.** Read-only: does not edit, push or merge. Reads: the PR description, the diff, the plan and ADRs it
cites. Owns: a findings list with severity (blocker / should-fix / nit). Must not skip: stating what was checked
(commands run, files read, what was not checked) and checking the Don'ts below. Do not reject for
parity divergence.

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
- Don't bump `rust-toolchain.toml` without re-checking the `kstring = "2.0.2"` pin in `Cargo.toml` (2.0.5 needs
  Rust 1.96); remove the pin only with the bump.
- Don't run tests or experiments against the real home directory or a real project's `.orchestry`.
- Don't flip the repo public, add remotes, or paste private names (taps, hosts, paths) anywhere in the repo or PRs.
- Don't add a dependency without the §4.8 check; add an ADR when it is load-bearing.
- Don't edit `docs/TRAITS-FROZEN.md` without an ADR.
- Don't push to `master`, force-push shared branches, or merge your own PR.
- Don't skip hooks or silence clippy with `#[allow]` without a reason comment.
