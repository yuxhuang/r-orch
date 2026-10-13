# CLI test map

What is ported from `test/unit/cli/**`, what is covered elsewhere, and what is
still waiting on another work package. Keep this current: it is the only place
that says *why* a TS test file has no Rust counterpart.

## Ported

| TS file | Cases | Rust home |
|---|---|---|
| `cli-lazy-routing.test.ts` | routing, container tiers, help fast path | `src/routing.rs` (unit) + `tests/routing.rs` (through the binary) |
| `create-context.test.ts` | `NO_COLOR` presence, `TERM=dumb`, project-root walk | `src/context.rs` |
| `output-lazy-chalk.test.ts` | icons, colours, table padding, formatters | `src/output.rs` |
| `commands-init.test.ts` | validation, idempotency, files written, adapter detection, the multi-adapter prompt | `src/commands/init.rs` + `tests/routing.rs` |
| `commands-doctor.test.ts` | both tiers, check list, the two header lines before `--json`, always exit 0 | `src/doctor.rs` + `tests/routing.rs` |
| `commands-logs.test.ts` | five routes, fetch sizes, display slices, four `--json` payloads, `--follow` shapes | `src/logs.rs` + `tests/routing.rs` |
| — (no TS unit file; §6 + §14.1) | `status` human block, `{tasks, agents, state}`, `claimed` as an **array** | `src/commands/status.rs` + `tests/routing.rs` |
| `commands-logs-since-all.test.ts` | the `--since`-only route, its run window and its 20-run cap | `src/logs.rs` |
| `commands-serve.test.ts` | `--log-format` validation, `--tick-interval` parsing, `--log-file` append, subscribe-before-watch | `src/serve/mod.rs` + `tests/routing.rs` |
| `serve-once-runner.test.ts` | the terminal rule, the empty-project exit, `has_failed` vs `all_done` | `src/serve/once.rs` |
| `structured-logger.test.ts` | record order, idle throttle, `agent:output` gating, the never-logged set, text format | `src/serve/logger.rs` |
| `disk-observer.test.ts` | tick shape, start/complete pairs, offsets, eviction | `src/serve/disk_observer.rs` |
| `disk-observer-integration.test.ts` | tailing a live JSONL, partial lines, corrupt lines | `src/serve/disk_observer.rs` |
| `editor.test.ts` | task buffer round trip, frontmatter parsing | `src/editor.rs` |
| `editor-agent.test.ts` | agent buffer, comment stripping, role-always-set | `src/editor.rs` |

## Deliberately not ported

| TS file | Why |
|---|---|
| `postinstall.test.ts` | `scripts/postinstall.cjs` patches Ink's caches and installs a Claude Code skill from an npm lifecycle hook. There is no npm package and no Ink; PARITY.md §13 puts it out of scope. |

## Waiting on another work package

These commands **parse** — the tree is complete, so V1's surface check is
honest — and fail with `… is not wired yet` rather than succeeding silently. A
scenario is added with each one, not before.

| Command | Needs |
|---|---|
| `org *` | org deploy (§18.2) |
| `task show/edit/cancel/approve/reject/retry` | WP-4 `TaskService` methods; `task cancel` additionally needs the orchestrator for an `in_progress` task |
| `agent status/edit/remove/autonomous` | WP-4 `AgentService` methods |
| `run` (`--all`, `--watch`, a single task) | WP-4 orchestrator. The argument check (`exit 2`) is wired already |
| `serve` (the watch itself) | WP-4 orchestrator. The logger, the `--once` rule, `--log-file`, `--tick-interval` and the disk observer are all built and tested; `serve::run_with` takes an orchestrator through `ServeOrchestrator` |
| `tui` | WP-6, including the bare-`orch` first-run path |
| `doctor`'s service | WP-4 may move the checks into `DoctorService`; the report type and renderer here are the target shape |

## Covered by the parity harness rather than a unit test

`fixtures/scenarios/` compares both binaries byte for byte, which covers what
a unit test cannot: the exact stdout of `init`, `task add`, `task list`,
`agent list`, the `_index.json` ordering, and the YAML cross-parse.

| Scenario | What it pins |
|---|---|
| `walking-skeleton*` | V39: init / agent add / task add / task list, in both cross-compat directions |
| `index-tie-order*` | the `_index.json` last-write order under a re-save (PARITY.md §14.4), in both directions |
| `cmd-status-and-doctor` | `status`/`doctor` human and `--json`, including the masked `state.claimed` (§14.1) and doctor's two header lines |
| `cmd-config*`, `cmd-context*` | the config/context stores through the CLI: compact `--json`, the dropped `value` key, `context get`'s exit-0 miss, the unclamped TTL column |
| `cmd-goal*`, `cmd-team` | the goal state machine's two exit codes out of one subcommand, the team membership rules, `disband` leaving the file readable |
| `cmd-msg*` | the fan-out shape of `send`/`broadcast`, the unknown-team no-op, the disabled-recipient filter, and body/subject trimming |
| `cmd-logs`, `cmd-init`, `cmd-ascii-and-quiet`, `cmd-exit-codes-uninitialized` | from master's corpus; green as of this work package |

Two notes on the harness as it now stands:

- **A scenario's `requires` is gated on *wired*, not *declared*.** WP-5a
  declares the whole command tree, so `<cmd> --help` exits 0 for every command
  and is no longer a usable probe: the gate is `WIRED_COMMANDS` in
  `xtask/src/harness.rs`, intersected with the `--help` check. Add a command
  there in the same commit that wires it, and the scenarios waiting on it go
  live.
- **`state.claimed` in `status --json` is the corpus's one masked field**
  (§14.1). `xtask/src/harness.rs`'s `claimed_mask` removes it from the
  comparison and `claimed_matches_state_file` asserts r-orch's array against
  the `state.json` the run wrote — or, when `init` left none, against the
  default empty set, which is the only correct report in that case.
