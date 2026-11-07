# 0011. A stdio MCP server in `orch-cli`, built on rmcp

- Status: proposed
- Date: 2026-11-07

## Context

The owner asked for a stdio MCP component so that other agents can talk to orch through MCP
([spec](../specs/2026-11-07-mcp-server.md), [plan](../plans/2026-11-07-mcp-server.md)). It adds a CLI contract
(`orch mcp`), a wire contract (tool names, arguments and results) and a load-bearing dependency, so it needs a
record.

Facts that forced the shape:
- Every one-shot command is an engine service call followed by printing, and `--json` prints the serialized
  domain value. A tool can be the same service call without the printing, in-process.
- The light engine tier (`build_light`) builds services without processes, adapters or the orchestrator lock. An
  `Engine` caches the project configuration and service defaults when it is built.
- Several commands print only a message (task cancel/approve/reject/retry, agent enable/disable/remove, the team
  membership commands, `config set`, `context delete`); `doctor --json` is prefixed by a human header.
- `orch task cancel` on a running task builds the full engine to kill the agent.
- Pausing a goal cancels every `todo`/`retrying` autonomous task of the goal's assignee, in any goal
  (`goal_service.rs`, `cancel_pending_autonomous_tasks`).
- Reads can write (index rebuild, context expiry), and the stores have no whole-record compare-and-swap
  (ARCHITECTURE §2.1).
- rmcp 3.9 is the official MCP Rust SDK: Apache-2.0, MSRV 1.88, bounded stdio framing (16 MiB lines). Its default
  argument decoding turns schema errors into tool results, not JSON-RPC invalid params.

## Decision

1. **Adopt `rmcp` 3.9** (default features off; `server`, `macros`, `transport-io`) and `schemars` 1.2 for argument
   schemas, under the library-first policy (ARCHITECTURE §4.8, §4.5.2). No hand-written JSON-RPC, lifecycle or
   framing.
2. **`orch mcp` is a subcommand in `orch-cli`**, a child process of its client over stdin/stdout. Tools and
   resources only; no prompts, subscriptions, HTTP transport or authentication. The crate graph is unchanged.
3. **In-process, light tier, fresh `Engine` per call.** The project root and identity are resolved once at start;
   each admitted call builds a new light `Engine`, so configuration written by any process applies to the next
   call. The server never builds the full engine and never takes the orchestrator's PID lock. One store operation
   runs at a time per server (16 admitted, FIFO, 10 s wait); `doctor` has its own bounded lane.
4. **One typed tool per CLI operation** (50 tools), with `deny_unknown_fields` argument structs, schemas from
   `schemars`, all four MCP annotations, and explicit decoding that returns JSON-RPC `-32602` for any schema
   violation. Results are the `--json` value unwrapped. Commands without a JSON value get documented MCP-only
   projections (the entity read back, or `null`); the CLI's output is not changed.
5. **Modes fixed at launch**: default (read + change, 45 tools), `--read-only` (18) and `--allow-destructive`
   (adds `task_cancel`, `goal_delete`, `agent_remove`, `team_disband`, `context_delete`). `goal_status` with
   `status=paused` or `force=true` is refused outside `--allow-destructive`, before the engine is built. The two
   flags conflict (usage error, exit 1).
6. **Identity is attribution**: `--agent` over `ORCH_AGENT_ID` sets the default sender and inbox; it authorizes
   nothing. `task_cancel` is store-only. `doctor` is the one tool that spawns processes (bounded probes).
7. **Concurrency is documented, not repaired here.** The server is one more non-transactional mutator, with the
   same hazards as two `orch` processes; recovery is in the RUNBOOK. Storage repair is separate work.

Rejected:
- **A subprocess wrapper over `orch --json`.** Process cost per call, stdout anomalies (the `doctor` header), no
  JSON for some commands, and identity and stdin problems.
- **A generic `orch_exec(argv)` tool.** Weak schemas, no per-operation mode gating, and a general command escape
  hatch.
- **A daemon or socket service.** A new lifecycle and a new authorization boundary for no gain over stdio.
- **One long-lived `Engine`.** Cheaper, but serves stale configuration and defaults after any write; rebuilding
  only after `config_set` still misses external edits.
- **The full engine tier.** Needs the PID lock, processes and adapters; conflicts with a running orchestrator.
- **rmcp's tool macros with their default decoding.** Schema errors would become tool results; a second decode
  path to fix that is less direct than one explicit registry.
- **A build-time plugin inventory (linkme, build script, per-group features).** Permanent complexity to solve a
  one-time build-ordering problem.
- **Gating only `force`.** Ordinary pause would keep a cross-goal cancellation in default mode.

## Consequences

- MCP clients (Claude Code, Codex, Cursor, the Inspector) can drive a project with typed tools, and agents orch
  spawns can use it with inherited `ORCH_AGENT_ID` and no per-project pin ([manual](../manual/17-mcp-server.md)).
- Every CLI change that adds an operation or flag must update the tool catalogue or its documented exclusion; the
  contract test (`crates/orch-cli/tests/mcp_contract.rs`) catches drift. Tool names and argument meanings are a
  compatibility surface: a breaking change needs a migration note in the CHANGELOG.
- A lead-type client that pauses goals must run `--allow-destructive`.
- Per-call engine builds cost a configuration read each time and allow one store operation at a time per server;
  fine at the documented scale (ARCHITECTURE A1), unmeasured beyond it.
- Concurrent MCP sessions widen how often the existing store races can be hit; they are not fixed. A broken config
  written through `config_set` breaks every call until the file is restored by hand.
- `rmcp` pulls in a second `sha2` major alongside `orch web`'s, until axum moves.
- Rollback is removing the client registration; state formats do not change, and completed mutations stay.
