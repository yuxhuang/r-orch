# CLI test map

Where `orch-cli` behaviour is tested. Keep this current.

| Area | Cases | Rust home |
|---|---|---|
| Routing | routing, container tiers, help fast path | `src/routing.rs` (unit) + `tests/routing.rs` (through the binary) |
| Context | `NO_COLOR` presence, `TERM=dumb`, project-root walk | `src/context.rs` |
| Output | icons, colours, table padding, formatters | `src/output.rs` |
| `init` | validation, idempotency, files written, adapter detection, the multi-adapter prompt | `src/commands/init.rs` + `tests/routing.rs` |
| `doctor` | both tiers, check list, the two header lines before `--json`, always exit 0 | `src/doctor.rs` + `tests/routing.rs` |
| `logs` | five routes, fetch sizes, display slices, four `--json` payloads, `--follow` shapes | `src/logs.rs` + `tests/routing.rs` |
| `status` | human block, `{tasks, agents, state}`, `claimed` as an **array** | `src/commands/status.rs` + `tests/routing.rs` |
| `logs --since` | the `--since`-only route, its run window and its 20-run cap | `src/logs.rs` |
| `serve` options | `--log-format` validation, `--tick-interval` parsing, `--log-file` append, subscribe-before-watch | `src/serve/mod.rs` + `tests/routing.rs` |
| `serve --once` | the terminal rule, the empty-project exit, `has_failed` vs `all_done` | `src/serve/once.rs` |
| Structured logger | record order, idle throttle, `agent:output` gating, the never-logged set, text format | `src/serve/logger.rs` |
| Disk observer | tick shape, start/complete pairs, offsets, eviction | `src/serve/disk_observer.rs` |
| Disk observer, live | tailing a live JSONL, partial lines, corrupt lines | `src/serve/disk_observer.rs` |
| Task editor | buffer round trip, frontmatter parsing | `src/editor.rs` |
| Agent editor | buffer, comment stripping, role-always-set | `src/editor.rs` |

## Not tested

There is no installer script and no npm lifecycle hook, so there are no install-time tests.

## Notes on `serve` and `run --watch`

`run --watch` and `serve` never return, so tests stop them with a signal (SIGTERM, then wait) and inspect the
tree they left, which is also the regression test for a shutdown that hangs. The `heap_mb` value of
`orchestrator:tick` records and clock times in `run`/`logs` output vary between runs, so tests do not assert
exact values for them.
