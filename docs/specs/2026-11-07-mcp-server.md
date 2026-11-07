# Stdio MCP server (`orch mcp`)

status: approved
owner: orchestrator (lead), for the repository owner
date: 2026-11-07
plan: to be written (`docs/plans/2026-11-07-mcp-server.md`; link it when the plan lands)

## Source
> Based on the orch architecture and all the commands, use your best judgement and capability, to add a stdio
> MCP component to orch (such as a subcommand `mcp`) to allow other agents to communicate to the orch
> orchestrator through the MCP aspect.

Items, split from the request above without rewording:
1. "Based on the orch architecture and all the commands, use your best judgement and capability,"
2. "to add a stdio MCP component to orch (such as a subcommand `mcp`)"
3. "to allow other agents to communicate to the orch orchestrator through the MCP aspect."

Owner's approval reply, 2026-11-07 (approves R1–R12 and answers the three gate questions):
> tier=mid stance=0.5 pr=yes q1=yes q2=use recommendation q3=adopt rmcp

The questions it answers, as put in the approval request:
- **q1** Mutating tools on by default: read *and* mutate tools are registered by default; destructive tools only
  with `--allow-destructive`; `--read-only` registers read tools only. Answer: yes (R4).
- **q2** Identity: `orch mcp --agent <id>` or `ORCH_AGENT_ID` sets the default sender and inbox; without it,
  sending a message needs an explicit `from`. Answer: the recommendation (R6).
- **q3** Dependency: adopt `rmcp` 3.9 (the official MCP Rust SDK) under the library-first policy, with an ADR.
  Answer: adopt (R10).

## Requirements

| Id | Requirement | Source | Priority | Acceptance check (test or command; platform) |
|---|---|---|---|---|
| R1 | When started as `orch mcp`, orch shall run an MCP server over stdio (JSON-RPC 2.0 framing per the MCP spec version `rmcp` 3.9 implements): stdout carries protocol frames only and logs go to stderr; it resolves the project from the working directory, `--project-root <path>` overrides that, and it refuses with exit 3 when the project is not initialised. | item 2 | must | `crates/orch-cli/tests/mcp_stdio.rs`: initialize handshake, stdout/stderr discipline, exit 3 uninitialised (cwd and `--project-root`); Linux CI + macOS |
| R2 | The server shall expose a tool for every one-shot command that reads or mutates project state: task add/list/show/assign/cancel/approve/reject/retry/update/patch; goal add/list/show/status/update/delete; agent list/show (`status`)/add/update/patch/enable/disable/autonomous/remove; msg send/broadcast/inbox/list; context set/get/list/delete; team create/list/show/join/leave/add-task/set-lead/disband; status; logs (run events); doctor; config get/set; remote list/status. Tool inputs mirror the CLI flags and results are the same JSON the command prints with `--json`. | items 1, 3 | must | table-driven test: `tools/list` ⊇ the catalogue; per-tool round-trip tests on a temp project comparing with `orch --json <cmd>`; Linux CI + macOS |
| R3 | The server shall not expose interactive, long-running, process-spawning or host-mutating commands as tools; each is listed with its reason in [R3 exclusions](#r3-exclusions). | item 1 | must | catalogue test asserts every excluded name is absent in all three modes (R4); Linux CI + macOS |
| R4 | Destructive tools (agent remove, goal delete, team disband, context delete, task cancel) shall be registered only with `--allow-destructive`, and `--read-only` shall register read tools only; every tool shall carry `readOnlyHint`, `destructiveHint` and `idempotentHint` annotations consistent with that classification. | approval q1 | must | `tools/list` tests over default, `--read-only` and `--allow-destructive`, including the annotations; Linux CI + macOS |
| R5 | The server shall serve the resources `orch://status`, `orch://tasks`, `orch://tasks/{id}`, `orch://goals`, `orch://goals/{id}`, `orch://agents`, `orch://agents/{id}`, `orch://context` and `orch://inbox/{agent}` as `application/json`, listed by `resources/list` and `resources/templates/list`. | item 3 | should | resource tests: list, templates list, read of each URI on a temp project, unknown id; Linux CI + macOS |
| R6 | When started with `--agent <id>` or with `ORCH_AGENT_ID` set, the server shall use that agent as the default `from` of msg send/broadcast and the default inbox agent, and the `initialize` result's `instructions` shall name the agent and the project; without either, `msg_send` shall require an explicit `from`. | approval q2; item 3 | must | tests with the flag, with the env var, and with neither; Linux CI + macOS |
| R7 | When a tool call fails, the server shall return a tool result with `isError: true` carrying the CLI's error message and hint for an `orch_core::Error`, a JSON-RPC invalid-params error for a schema violation, and shall never panic or exit on a bad call. Inputs get the same validation as the CLI; frame size is bounded by `rmcp`. | item 3 | must | malformed-input tests (bad JSON, unknown tool, wrong types, unknown ids, oversized fields) followed by a successful call on the same session; Linux CI + macOS |
| R8 | The server shall exit cleanly on stdin EOF and on SIGTERM/SIGINT, take no project lock, and the manual and RUNBOOK shall state the ARCHITECTURE §2.1 concurrent-mutator hazard of running it alongside `orch run --watch`. | item 2 | must | EOF and signal tests in `crates/orch-cli/tests/mcp_stdio.rs` (exit status, no leftover child); docs check by the fact-checker; Linux CI + macOS |
| R9 | The PR shall ship an MCP chapter in `docs/manual/` (client configs: Claude Code `claude mcp add orch -- orch mcp`, Codex `config.toml`, generic), a README section, a CHANGELOG `[Unreleased]` entry, ADR `docs/decisions/0011-mcp-server.md`, this spec and its plan, ARCHITECTURE §3.3 module map and §4 dependency entry, and a RUNBOOK entry. | plan (AGENTS.md documentation duties) | must | fact-check report on the PR head; every listed file present in `git diff --stat main..HEAD`; macOS |
| R10 | `rmcp` shall be adopted under ARCHITECTURE §4.8 (maintenance, MSRV, licence recorded), with `cargo deny check` and `cargo xtask crate-graph` green. | approval q3 | must | `cargo deny check`; `cargo xtask crate-graph`; Linux CI + macOS |
| R11 | The CI set shall be green locally on macOS and on Linux CI, with a binary-level stdio test and unit tests per tool group. | plan (AGENTS.md pre-PR check) | must | `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`; `cargo xtask crate-graph`; `cargo deny check`; Linux CI + macOS |
| R12 | A real MCP client (Claude Code or MCP Inspector) shall initialise against `orch mcp`, list its tools and run `task_add` then `task_show` on a temp project. | item 3 | should | qa-engineer capture (transcript + `orch --json task show <id>`); macOS only, env-limited to the owner's Mac |

### R3 exclusions

Every row is `won't` (no tool in any mode).

| Command | Reason |
|---|---|
| `run` | Long-running and spawns agent processes; orchestrator control is a non-goal. |
| `serve` | Long-running headless daemon that dispatches agents. |
| `tui` | Interactive full-screen terminal UI; needs a TTY that stdio MCP does not have. |
| `web` | Long-running, listens on a socket. |
| `init` | Creates a project; the server requires an initialised one (R1). |
| `update` | Host-mutating: downloads and installs a new `orch` binary. |
| `agent shop` | Interactive template picker by default. |
| `org` (`list`, `template …`, `deploy`) | Bulk agent creation from templates and template authoring on disk; deferred with `agent shop`. |
| `task edit`, `agent edit` | Interactive: open `$EDITOR`. `update`/`patch` cover the same change (R2). |
| `config edit` | Interactive: opens `$EDITOR`. |
| `config global` (`get`, `set`, `show`) | Reads and writes `~/.orchestry/global.yml`, host-wide state outside the project. |
| `remote add`, `remote remove` | Change remote-peer links; remote-peer control is a non-goal. |
| `remote listen` | Changes the peer listen setting, and so the network exposure of later engines. |
| `logs --follow` | Long-running live stream; `logs` without `--follow` is a tool (R2). Same class as R3. |

## Non-goals
- HTTP or SSE transport.
- Authentication: stdio inherits the permissions of the user who launched it.
- MCP prompts.
- Exposing `run` or other orchestrator control.
- Remote-peer control.
- Changing on-disk formats.

## Constraints
- On-disk: unchanged. Tools write through the same services as the CLI.
- CLI: one new subcommand, `orch mcp`, with `--project-root`, `--agent`, `--read-only` and `--allow-destructive`.
  Existing commands and their `--json` output are unchanged.
- `--json`: tool results reuse the existing `--json` shapes; no new envelope.
- Peer protocol: unchanged.
- Concurrency: the server is one more mutator alongside the CLI and `orch run --watch`, the same hazard as a
  CLI call (ARCHITECTURE §2.1); it takes no lock (R8).
- Platforms: macOS and Linux.

Security checklist:
- Default bind and exposure: no socket; stdio only, to the parent process that spawned it. No authentication;
  the client gets the launching user's access to the project.
- Bounds on input size, count and rate: frame size bounded by `rmcp`; field validation as the CLI (R7). No rate
  limit: the client is the local parent process.
- Permissions of files it writes: the project's existing store files, written by the same code paths as the
  equivalent CLI command; no new files.
- Secrets and environment passed to children: the `doctor` tool runs the same adapter probes as `orch doctor`;
  no other tool spawns a child.
- Untrusted text rendered to a terminal or passed to a shell: tool arguments come from an LLM client and are
  untrusted. They are never passed to a shell and never rendered to a terminal; stdout carries JSON frames only.
  Text stored through tools (messages, context, task descriptions) reaches agent prompts exactly as the same
  text stored through the CLI does; no new path.

## Verification environment
- Local (macOS) and Linux CI: R1–R8, R10, R11. Tests drive the built binary over stdin/stdout with a raw
  JSON-RPC client on a temp project and call no real AI CLI.
- R9: fact-checker on the PR head (macOS).
- R12: needs a real MCP client on the owner's Mac (Claude Code is installed there; MCP Inspector needs `npx` and
  network). Run by qa-engineer. Listing tools and calling `task_add`/`task_show` through MCP Inspector spends no
  model tokens; driving them through a Claude Code session does.
- No dashboard behaviour changes; no tmux test is needed.

## Open questions
- **`config update` and `config patch`.** They are one-shot and mutate project state, but appear in neither R2
  nor R3. Best guess: add them to R2 as mutating tools (they mirror `task`/`agent` `update`/`patch`). Adding a
  row is allowed after approval; the owner confirms when reviewing the PR.
- **`doctor` and R3's "process-spawning" class.** `orch doctor` spawns bounded adapter probes, yet R2 lists it as
  a tool. Reading: R3 excludes commands that spawn agent runs or long-lived children, not bounded probes. The
  design states the probe bounds.
- **`--read-only` together with `--allow-destructive`.** Not specified. Best guess: refused as a usage error
  (exit 1, as every orch usage error).

## Verification
Filled in by the PR that completes the work.

| Id | Result (`pass` / `env-limited` / `waived by owner`) | Commit the check ran at | Platform |
|---|---|---|---|
| R1 | | | |
| R2 | | | |
| R3 | | | |
| R4 | | | |
| R5 | | | |
| R6 | | | |
| R7 | | | |
| R8 | | | |
| R9 | | | |
| R10 | | | |
| R11 | | | |
| R12 | | | |

Defects found outside the requirements:
- none yet
