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

## Status

Waves as run, in order. Ids in parentheses are the orch task ids of goal `goal_WpQVmPg`.

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
| C: review | Checkpoint CP2: freeze the integration commit, launch the review board (GATE) | `tsk_Brzri1x` | pending |

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
