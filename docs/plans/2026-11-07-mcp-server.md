# Stdio MCP server (`orch mcp`)

status: active
owner: orchestrator (lead)
date: 2026-11-07
spec: [../specs/2026-11-07-mcp-server.md](../specs/2026-11-07-mcp-server.md)

## Goal
`orch mcp` serves an initialised project to MCP clients over stdio, with a typed tool for every one-shot
project command (R2), the `orch://` resources (R5), three launch modes (R4), attribution by agent identity (R6),
CLI-equivalent validation and errors (R7) and clean lifecycle without the orchestrator lock (R8), documented (R9)
and green on macOS and Linux CI (R10, R11), with a real-client smoke test (R12).

PR 2, the follow-up of goal `goal_h-gIBCW` (owner, 2026-11-08), closes PR #69's accepted residuals and follow-ups
except storage repair: goal-scoped pause cancellation with `goal_status status=paused` in default mode (R4
re-amended, R13), value-free invalid-params diagnostics (R14), non-UTF-8 project roots in `task_add` attachments
(R15), a stdio test of qualified resources (R16), SIGHUP named in R8, a host-git-independent `yaml_contract`
fixture, and the docs (store-only `task_cancel`, design assumptions, the client-version claim).

## Non-goals
HTTP/SSE transport, authentication, MCP prompts, orchestrator or remote-peer control, on-disk format changes,
store repair for concurrent writers (follow-up), automatic injection of the server into spawned agents. See the
spec.

## Context
- Branch `feature/mcp-server` from `main` at `8e39f2f` (v0.11.1).
- Decision record: [ADR 0011](../decisions/0011-mcp-server.md). User manual:
  [chapter 17](../manual/17-mcp-server.md). Concurrency: [ARCHITECTURE §2.1](../ARCHITECTURE.md#21-a2-in-detail--the-concurrency-story-is-weaker-than-single-writer).
- Research (rmcp 3.9 API, client configuration formats) was fact-checked; a design was red-teamed and revised.
  Two lead decisions completed the spec: decision-1 (MCP-only result projections, store-only `task_cancel`, the
  `doctor` probe exception, `config update`/`patch` included, `file: "-"` refused) and decision-2 (pause and force
  gated behind `--allow-destructive`, concurrent-store residual risk accepted with documentation, conflicting mode
  flags are a usage error). The research, design and rulings are kept with the team's working files, not in the
  repo; the spec records each ruling.

## Design

**Placement.** A new `orch-cli::mcp` module tree (ARCHITECTURE §3.3.1); no change to `orch-core`, `orch-engine`,
`orch-infra` or the crate graph. `orch mcp` is dispatched before the ordinary runtime is built and is exempt from
the pre-parse project guard, so `--project-root` works from an uninitialised directory; it resolves its own root
(exit 3 when there is none).

**Per call.** rmcp's stdio transport and router; an application `Registry` that records each tool's name,
`schemars` schema, four annotations and class, decodes arguments strictly (JSON-RPC `-32602`), runs
`ToolArgs::validate` (identity, form and mode policy) before any engine is built, then admits the call: a store
lane (16 permits, FIFO mutex, 10 s wait) or, for `doctor` only, a diagnostic lane (2 permits, 120 s). Each admitted
call builds a fresh light `Engine` and calls the service directly; definition mutations and `config set` reuse
print-free helpers extracted from `commands::{task, agent, config}`.

**Tool groups.** Nine leaf modules (`mcp/tools/{task,goal,msg,context}.rs`: 24 tools; `{agent,team,logs,config,
remote}.rs`: 24 tools) plus `status` and `doctor` in `mcp/system.rs`: 50 tools; 45 in default mode, 18 read-only.
Leaves were seeded as no-op `register` functions first, so the skeleton compiled with every module declared and no
builder edited another's file.

**Rejected** (details in ADR 0011): subprocess `orch --json` wrapper; generic `orch_exec`; daemon/socket; one
long-lived engine; full engine tier; rmcp macro default decoding; build-time plugin inventory.

## Tasks

All tasks land in one feature PR (PR 1). It may exceed the ~3,000-line guideline; the lead records the reason in
the PR and reviews source in slices under 800 lines. Orch task ids are in [Status](#status).

| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| P0 | Add `rmcp` 3.9 and `schemars` 1.2 | R10 | `Cargo.toml`, `Cargo.lock`, `deny.toml`, `crates/orch-cli/Cargo.toml` | — | `cargo deny check`; `cargo xtask crate-graph` | 1 |
| SPEC | Spec, reconciled with decision-1 and decision-2 | R1–R12 | `docs/specs/2026-11-07-mcp-server.md` | — | lead review | 1 |
| T0 | Failing-first stdio harness | R1 | `crates/orch-cli/tests/mcp_stdio.rs`, `crates/orch-cli/tests/mcp_common/mod.rs` | — | `cargo test -p orch-cli --test mcp_stdio` | 1 |
| B0a, B0b | Seed the nine tool leaves (no-op `register`) | — | `crates/orch-cli/src/mcp/tools/*.rs` (leaf files) | — | consumed by B1's build | 1 |
| B1 | Compiled skeleton: CLI wiring, server, registry, modes, identity, admission, projections, resources, `status`/`doctor`, print-free helpers | R1, R4–R8 | `crates/orch-cli/src/mcp.rs`, `crates/orch-cli/src/mcp/{server,identity,mode,registry,projections,resources,system}.rs`, `mcp/tools.rs`, `tree.rs`, `lib.rs`, `routing.rs`, `commands/{mod,task,agent,config}.rs`, `doctor.rs` | P0, B0a, B0b | `cargo test -p orch-cli mcp::` | 1 |
| B2a | Task, goal, message and context tools (24) | R2, R4, R6, R7 | `crates/orch-cli/src/mcp/tools/{task,goal,msg,context}.rs` | B1 | `cargo test -p orch-cli mcp::tools`; `cargo check -p orch-cli` | 1 |
| B2b | Agent, team, logs, config and remote tools (24) | R2, R4, R7 | `crates/orch-cli/src/mcp/tools/{agent,team,logs,config,remote}.rs` | B1 | `cargo test -p orch-cli mcp::tools`; `cargo check -p orch-cli` | 1 |
| T1 | Catalogue, mode, contract and lifecycle tests; clap drift fixture | R1, R3–R8 | `crates/orch-cli/tests/{mcp_stdio,mcp_contract}.rs`, `crates/orch-cli/tests/mcp_common/mod.rs` | B1 | `cargo test -p orch-cli --test mcp_stdio --test mcp_contract` | 1 |
| T2 | Per-tool behaviour tests: CLI parity, gates, config freshness and poison, two-process creates, worker root/identity matrix | R2, R4, R6–R8 | `crates/orch-cli/tests/mcp_tools.rs` | B2a, B2b, T1 | `cargo test -p orch-cli --test mcp_tools` | 1 |
| W1 | Docs draft: manual chapter 17, command reference, README, CHANGELOG, ADR 0011, this plan, ARCHITECTURE, RUNBOOK | R8, R9 | `docs/manual/{17-mcp-server,README,14-command-reference}.md`, `docs/manual/tools/command_reference.py`, `README.md`, `CHANGELOG.md`, `docs/decisions/{0011-mcp-server,README}.md`, `docs/plans/2026-11-07-mcp-server.md`, `docs/ARCHITECTURE.md`, `docs/RUNBOOK.md` | B1, SPEC | links resolve; no personal machine paths (AGENTS.md rule 6); `docs/manual/tools/gen-command-reference.sh` output committed | 1 |
| I | Integration: full CI set, aggregate fixes, review packet | R11 | MCP production and test files | T2 | the CI set (below) | 1 |
| W2 | Verify every doc example against the integration commit | R9 | as W1 | I, W1 | examples re-run on temp projects; fact-check | 1 |
| GATE | Security, resilience, code, docs and real-client QA reviews; fixes | R7, R8, R9, R12 | per finding | W2 | reviewer verdicts; R12 capture | 1 |

### PR 2: follow-ups (goal `goal_h-gIBCW`)

Branch `fix/mcp-followups` from `main` at `6943c8b` (base `20e81f9` with the AGENTS.md row). D10 storage races
and `crates/orch-infra` storage locking are out of scope.

| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| F-B1 | Scope pause cancellation to the paused goal; `goal_status status=paused` in default mode, `force=true` gated; description and `initialize` instructions | R4, R13 | `crates/orch-engine/src/goal_service.rs`, `crates/orch-cli/src/mcp/tools/goal.rs`, `crates/orch-cli/src/mcp/server.rs`, `crates/orch-cli/tests/mcp_tools.rs` | — | `cargo test -p orch-engine goal_service`; `cargo test -p orch-cli --test mcp_tools` | 2 |
| F-B2 | Value-free invalid-params text; non-UTF-8 project root in `task_add` attachments; `task_cancel` liveness decision | R14, R15, R3 | `crates/orch-cli/src/mcp/{registry,tools/task}.rs`, `crates/orch-cli/src/lib.rs`, `crates/orch-cli/Cargo.toml`, `Cargo.toml`, `Cargo.lock` | — | `cargo test -p orch-cli mcp::` | 2 |
| F-T1a | Seed a commit in the `yaml_contract` fixture; qualified-resource stdio test; SIGHUP coverage audit | R8, R16 | `crates/orch-cli/tests/{mcp_stdio,yaml_contract}.rs`, `crates/orch-cli/tests/mcp_common/**` | — | `cargo test -p orch-cli --test mcp_stdio --test yaml_contract` | 2 |
| F-W1 | Spec, plan, ADR 0011 (accepted), manual 17, README, CHANGELOG, internal orchestration notes | R4, R8, R9, R13–R16 | `docs/**`, `README.md`, `CHANGELOG.md` | — | links resolve; no stale cross-goal wording (`grep -rn "other goals\|across all goals\|across goals"`) | 2 |
| F-FC | Fact-check the client-version load claim in manual 17 | R9 | reader | — | fact report | 2 |
| F-RT | Red-team the goal-scoped pause decision | R4, R13 | reader | — | report; lead adjudicates at CP1 | 2 |
| F-T1b | Stdio test of the R14 stderr under `RUST_LOG=warn` | R14 | as F-T1a | F-B2, F-T1a | `cargo test -p orch-cli --test mcp_stdio` | 2 |
| F-I | Integration: CI set, cold `./xtask/gate.sh`, review packet, frozen commit | R11 | as F-B1, F-B2, F-T1a | F-T1b, CP1 changes | the CI set | 2 |
| F-W2 | Client-version sentence from F-FC; docs reconciled with the frozen commit; Verification rows R4/R8 (amended), R13–R16; spec mirror | R9 | as F-W1 | F-I, F-FC | fact-check | 2 |
| F-GATE | Code, security, resilience and docs reviews, cold gate; fixes | R4, R7, R8, R9, R13–R16 | per finding | F-W2 | reviewer verdicts | 2 |

## Status

Waves as run, in order. Ids in parentheses are the orch task ids of goal `goal_WpQVmPg` (PR 1) and goal
`goal_h-gIBCW` (PR 2). States are a snapshot of the `goal_h-gIBCW` task graph at 2026-11-09 02:02 UTC, plus the
`tsk_2YEYhOL` row, which the commit that adds it completes. A row marked pending was still open then; this table
is not updated after that.

| Wave | Item | Task | State |
|---|---|---|---|
| A: research and design | Research rmcp 3.9 and client configs, fact-checked | `tsk_Rfhsl4g`, `tsk_0Dq6FPt` | done (team working files) |
| | Design v1, red team, rebuttal and design v2 | `tsk_3AthnJH`, `tsk_HYrE5s9`, `tsk_eEAHV_J` | done (team working files) |
| | Lead decision-1 (CLI parity and safety gaps) | `tsk_ykx4hSN` | done |
| | P0 | `tsk_rOxum7E` | implemented (`0412123`) |
| | SPEC | `tsk_8BVlLkR` | approved (`0972369`) |
| | T0 | `tsk_DrEEWc7` | implemented, failing-first (`d7c5237`) |
| | SPEC reconciled with decision-1 | `tsk_qBhlLf6` | done (`87a1a62`) |
| | Checkpoint CP1: adjudicate design v2, lead decision-2, create wave B | `tsk_iEG5dJg` (`tsk_KsG8tAT` cancelled and replaced) | done |
| B: build | SPEC reconciled with decision-2 | `tsk_IDM084s` | done (`50a56f1`) |
| | B0a, B0b | `tsk_sGmlgP1`, `tsk_qYsvC3z` | implemented (`8e01479`, `de67de4`) |
| | B1 | `tsk_uYLw07I` | implemented (`fb0f5b6`) |
| | B2b, then its security review | `tsk_cYZ7NCG`, `tsk_N0ew-Ms` | implemented (`5b06c5d`); review passed |
| | W1 | `tsk_YqgQ81m` | drafted (`5bdaab5`) |
| | T1 | `tsk_eozgGPK` | implemented (`e55e89c`) |
| | B2a | `tsk_aqrHEPr` | implemented (`b422994`) |
| | T2 | `tsk_Y1rc-mG` | implemented (`d0a134a`, `8bde6f0`) |
| | Fix: stalled `doctor` probe left alive after SIGTERM/EOF | `tsk_588rI6O` | fixed; committed in I |
| | Fix: `task_add` relative attachments resolve against the project root (D12) | `tsk_6HalWGX` | fixed in I; verified, no separate commit |
| | I | `tsk_FxWZnjY` | integrated (`00450c9`): full CI set green, 50/45/18 tools by mode |
| | W2 | `tsk_4SVACBX` | docs verified against `00450c9` |
| C: review | Checkpoint CP2: freeze the integration commit, launch the review board (GATE) | `tsk_Brzri1x` | done |
| | GATE, fix rounds, PR #69 | goal `goal_WpQVmPg` | merged as `679011d`; head `42d2808`, CI green (Linux x86_64) |
| D: PR 2, wave A | F-B1 | `tsk_uhm73p-`; default-mode instructions test `tsk_z4gifpJ` | implemented (`a9a8bf9`, `1e731c2`) |
| | F-B2 | `tsk_zxG0McJ` | implemented (`64d1e3e`); `task_cancel` liveness decided docs-only at CP1 |
| | F-T1a | `tsk_6_gi7MU` | implemented (`072d6b7`, `2b70c31`) |
| | F-W1 | `tsk_gp6Ry0K` | drafted (`5c83712`) |
| | F-FC | `tsk_tkh0_l3` | done: claim verified, dated sentence for F-W2 |
| | F-RT | `tsk_AoDE3ZO` | done: ship with changes (disclosures), for CP1 |
| | Checkpoint CP1: adjudicate wave A | `tsk_IUpXm7b` | done: RT1/RT2 accepted as disclosures (B1b) |
| E: PR 2, wave B and gate | F-T1b | `tsk_UhK4OPS` | implemented (`eb735ac`) |
| | B1b (CP1 disclosures, stale `--allow-destructive` help) and F-I | `tsk_WVInOdA` | integrated and code frozen (`514560d`) |
| | F-W2 | `tsk_NUnWRGF` | docs finished at the frozen code (`ca51c7e`) |
| | F-GATE: code, security, resilience, facts, cold gate at `ca51c7e` | `tsk_v5FUQHQ`, `tsk_AauPKD9`, `tsk_irHNbOB`, `tsk_vDRoMF_`, `tsk_K3GBJR8` | done: all five pass with conditions, none blocking |
| | Checkpoint CP2: governance verdicts | `tsk_rJCHWFp` | done: fix round authorised |
| F: PR 2, CP2 fix round | F1: value-free `logs.since` form errors (R14), then its docs | `tsk_zt0J71i`, then `tsk_AUiHMlQ` (replaces cancelled `tsk_0Q4didm`) | done: fix and tests `152c62c`; R14 row, manual 17, RUNBOOK and CHANGELOG reconciled (`08c3237`) |
| | C2: qualify pause dispatch wording in code text, then docs (with SRE-2, F5) | `tsk_XQxdkEP`, then `tsk_cM9TgND` (replace cancelled `tsk_DG05bju`, `tsk_Kyg4A2e`) | done: code text `c134e52`, docs `d3dace0` |
| | SRE-3: pin terminal mode in `yaml_contract` and `routing` glyph fixtures | `tsk_EDxaAMe` (replaces cancelled `tsk_4dskzEb`) | done (`1898dd3`) |
| | F2: non-UTF-8 attachment wording matches resolved-path behaviour | `tsk_yNNFDQg` | done (`d68002f`): R15 row, manual 17, CHANGELOG |
| | F3, first pass: this status table and the liveness decision | `tsk_CFNaK07` | done (`9f6ef6c`); remaining rows refreshed in wave H |
| | I2: integrate the fix round, full gate set, re-freeze | `tsk_MACcZj4` | done: full gate set run, re-frozen at `d3dace0`; no integration commit |
| G: PR 2, re-gate and PR | Re-reviews at the re-frozen SHA: code, security, resilience, facts | `tsk_QKJV45G`, `tsk_WBWDCzh`, `tsk_P-mM5Cp`, `tsk_3bHsl13` | done at `d3dace0`: code, security and resilience pass; facts pass with conditions (F3: this table) |
| | PR: push, open the single PR against `main`, Linux CI green | `tsk_5TRHZoX` | PR [#72](https://github.com/yuxhuang/r-orch/pull/72) opened at `d3dace0`; Linux CI green, run 42547054498; owner merges (squash) |
| | Checkpoint CP3: re-gate verdicts and PR | `tsk_65sIOO0` | done 2026-11-09: fix F3, then CP4 |
| H: PR 2, F3 fix and close | F3, second pass: refresh the remaining statuses after CP3 | `tsk_2YEYhOL` | done: the commit that adds this row, on top of `e8af27e` |
| | Push that commit to PR #72 (fast-forward), Linux CI green | `tsk_Ls5_1Ug` | pending |
| | Scoped fact re-check of this table at the final SHA | `tsk__eHJeUN` | pending |
| | Checkpoint CP4: final SHA, CI, F3 verdict, report | `tsk_rGkc9H4` | pending |

Decided at CP1 (`tsk_IUpXm7b`): `task_cancel` reports no liveness field and stays store-only. A live PID on the
lock is not proof that a live orchestrator will reconcile the cancel (one-shot holders, PID reuse, the race
between observing and acting), and the owner asked for docs only. The tool description and manual 17 state that
the tool does not stop an orphan agent process when no live orchestrator owns the run.

## Test plan
- The spec's acceptance checks, R1–R11 on macOS and Linux CI; R12 (real client: Claude Code or MCP Inspector) on
  macOS by QA, env-limited to the owner's Mac.
- Binary-level tests drive `orch mcp` over raw JSON-RPC on temp projects, each child with its own temp `HOME` and
  `ORCHESTRY_GLOBAL_CONFIG_PATH`; no real AI CLI runs.
- CI set: `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`;
  `cargo test --workspace`; `cargo xtask crate-graph`; `cargo deny check`. The cold `./xtask/gate.sh` runs before
  the PR, because this change crosses a process boundary and handles untrusted input.

## Rollback / compatibility
Additive: one subcommand and its dependency. `.orchestry/` formats and the peer protocol do not change, so older
binaries read the same state. To back out, remove the client registrations; mutations already made through the
server stay.

## Docs to update
README (MCP section), RUNBOOK (concurrent-writer recovery, config-poison restore), ARCHITECTURE (§2.1, §3.3,
§3.3.1, §4.5.2), CHANGELOG (`[Unreleased]`), ADR 0011, manual chapter 17 and the regenerated command reference.

## Open questions
- Store repair for concurrent writers (compare-and-swap for whole-record saves, context expiry under the index
  lock) is follow-up work outside this plan (decision-2, E2).
- The pre-existing synchronous open in `fs_utils::mutation_lock` is accepted for v1 and recorded as a follow-up
  (decision-1).
